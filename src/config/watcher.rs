use anyhow::Result;
use notify::{
    event::ModifyKind, Config as NotifyConfig, Event, EventKind, RecommendedWatcher,
    RecursiveMode, Watcher,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use super::Config;

/// Configuration change event
#[derive(Debug, Clone)]
pub enum ConfigEvent {
    /// Config file was modified and reloaded
    Reloaded(Arc<Config>),

    /// Config file was modified but reload failed
    ReloadFailed(String),
}

/// Hot reload configuration watcher (Envoy-style)
pub struct ConfigWatcher {
    /// Path to config file
    path: PathBuf,

    /// File watcher
    watcher: RecommendedWatcher,

    /// Event receiver
    event_rx: mpsc::Receiver<notify::Result<Event>>,

    /// Current config
    current: watch::Sender<Arc<Config>>,

    /// Debounce duration (avoid rapid reloads)
    debounce: Duration,
}

impl ConfigWatcher {
    /// Create a new config watcher
    pub fn new(path: impl AsRef<Path>, initial: Config) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let (event_tx, event_rx) = mpsc::channel(16);

        // Create file watcher
        let watcher = RecommendedWatcher::new(
            move |res| {
                let _ = event_tx.blocking_send(res);
            },
            NotifyConfig::default().with_poll_interval(Duration::from_secs(1)),
        )?;

        let (current, _) = watch::channel(Arc::new(initial));

        Ok(Self {
            path,
            watcher,
            event_rx,
            current,
            debounce: Duration::from_millis(500),
        })
    }

    /// Start watching for config changes
    pub fn start(&mut self) -> Result<()> {
        info!(path = %self.path.display(), "starting config watcher");

        self.watcher
            .watch(&self.path, RecursiveMode::NonRecursive)?;

        Ok(())
    }

    /// Subscribe to config changes
    pub fn subscribe(&self) -> watch::Receiver<Arc<Config>> {
        self.current.subscribe()
    }

    /// Get current config
    pub fn current(&self) -> Arc<Config> {
        self.current.borrow().clone()
    }

    /// Process events (call in a loop)
    pub async fn process_events(&mut self) -> Option<ConfigEvent> {
        // Wait for file event
        let event = self.event_rx.recv().await?;

        match event {
            Ok(event) => {
                // Only care about modify events
                if !matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Data(_))
                        | EventKind::Modify(ModifyKind::Any)
                ) {
                    return None;
                }

                debug!(paths = ?event.paths, "config file modified");

                // Debounce - wait a bit before reloading
                tokio::time::sleep(self.debounce).await;

                // Reload config
                match Config::load(&self.path) {
                    Ok(config) => {
                        let config = Arc::new(config);
                        info!(
                            listeners = config.listeners.len(),
                            clusters = config.clusters.len(),
                            "configuration reloaded"
                        );

                        let _ = self.current.send(config.clone());
                        Some(ConfigEvent::Reloaded(config))
                    }
                    Err(e) => {
                        warn!(error = %e, "configuration reload failed, keeping current config");
                        Some(ConfigEvent::ReloadFailed(e.to_string()))
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "file watcher error");
                None
            }
        }
    }

    /// Run the watcher loop
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                event = self.process_events() => {
                    if let Some(ConfigEvent::Reloaded(_)) = event {
                        metrics::counter!("smppd.config.reloads").increment(1);
                    } else if let Some(ConfigEvent::ReloadFailed(_)) = event {
                        metrics::counter!("smppd.config.reload_failures").increment(1);
                    }
                }
                _ = shutdown.changed() => {
                    info!("config watcher shutting down");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_config_reload() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");

        // Write initial config
        fs::write(
            &config_path,
            r#"
listeners:
  - name: default
    address: "0.0.0.0:2775"
clusters: []
routes: []
"#,
        )
        .unwrap();

        let initial = Config::load(&config_path).unwrap();
        let mut watcher = ConfigWatcher::new(&config_path, initial).unwrap();
        watcher.start().unwrap();

        assert_eq!(watcher.current().listeners.len(), 1);
    }
}
