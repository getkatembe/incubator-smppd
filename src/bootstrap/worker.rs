use std::sync::Arc;
use std::thread;
use tokio::runtime::Builder;
use tracing::{info, error, span, Level};

use super::shutdown::Shutdown;

/// Worker thread configuration
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Number of worker threads (0 = num_cpus)
    pub workers: usize,

    /// Thread stack size
    pub stack_size: usize,

    /// Thread name prefix
    pub name_prefix: String,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            workers: 0,
            stack_size: 2 * 1024 * 1024, // 2MB
            name_prefix: "smppd-worker".to_string(),
        }
    }
}

/// Worker thread handle
pub struct Worker {
    id: usize,
    handle: Option<thread::JoinHandle<()>>,
}

impl Worker {
    /// Spawn a new worker thread
    pub fn spawn(
        id: usize,
        config: &WorkerConfig,
        shutdown: Arc<Shutdown>,
    ) -> Self {
        let name = format!("{}-{}", config.name_prefix, id);
        let stack_size = config.stack_size;

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let handle = thread::Builder::new()
            .name(name.clone())
            .stack_size(stack_size)
            .spawn(move || {
                let span = span!(Level::INFO, "worker", id = id);
                let _enter = span.enter();

                info!("worker thread started");

                // Build single-threaded runtime for this worker
                let runtime = Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build worker runtime");

                // Signal ready
                let _ = ready_tx.send(());

                // Run until shutdown
                runtime.block_on(async {
                    let mut shutdown_rx = shutdown.subscribe();

                    // Wait for shutdown signal
                    loop {
                        tokio::select! {
                            _ = shutdown_rx.changed() => {
                                break;
                            }
                            // Worker would process tasks here
                            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                        }
                    }
                });

                info!("worker thread stopped");
            })
            .expect("failed to spawn worker thread");

        // Wait for worker to be ready
        let _ = ready_rx.recv();

        Self {
            id,
            handle: Some(handle),
        }
    }

    /// Get worker ID
    pub fn id(&self) -> usize {
        self.id
    }

    /// Join the worker thread
    pub fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            if let Err(e) = handle.join() {
                error!(worker_id = self.id, "worker thread panicked: {:?}", e);
            }
        }
    }
}

/// Worker threads (Envoy-style)
pub struct Workers {
    inner: Vec<Worker>,
}

impl Workers {
    /// Create workers from config
    pub fn new(config: WorkerConfig, shutdown: Arc<Shutdown>) -> Self {
        let num_workers = if config.workers == 0 {
            num_cpus::get()
        } else {
            config.workers
        };

        info!(workers = num_workers, "starting worker pool");

        let inner: Vec<Worker> = (0..num_workers)
            .map(|id| Worker::spawn(id, &config, shutdown.clone()))
            .collect();

        info!(count = inner.len(), "workers started");

        Self { inner }
    }

    /// Get number of workers
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Shutdown all workers
    pub fn shutdown(&mut self) {
        info!(count = self.inner.len(), "shutting down workers");

        for worker in &mut self.inner {
            worker.join();
        }

        info!("workers stopped");
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.shutdown();
    }
}
