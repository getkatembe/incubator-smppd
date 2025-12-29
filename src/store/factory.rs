//! Storage factory for creating storage backends.

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{StorageBackend, StoreConfig};

use super::{MemoryStorage, PersistentStorage, SharedStorage};

/// Resolve the data directory.
fn resolve_data_dir(config_path: Option<&std::path::Path>) -> PathBuf {
    if let Some(path) = config_path {
        if path.is_absolute() {
            return path.to_path_buf();
        }
        return std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path);
    }

    #[cfg(unix)]
    {
        if unsafe { libc::getuid() } == 0 {
            return PathBuf::from("/var/lib/smppd");
        }
    }

    dirs::data_dir()
        .map(|p| p.join("smppd"))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".local/share/smppd"))
                .unwrap_or_else(|| PathBuf::from("./data"))
        })
}

/// Create a storage backend based on configuration.
///
/// Returns a unified storage that handles both messages and feedback.
pub async fn create_storage(config: &StoreConfig) -> anyhow::Result<SharedStorage> {
    match config.backend {
        StorageBackend::Memory => {
            tracing::info!("using in-memory storage (volatile)");
            Ok(Arc::new(MemoryStorage::new()))
        }
        StorageBackend::Fjall => {
            let data_dir = resolve_data_dir(config.fjall.path.as_deref());
            std::fs::create_dir_all(&data_dir)?;
            tracing::info!(path = %data_dir.display(), "using persistent storage");
            Ok(PersistentStorage::open(&data_dir).await? as SharedStorage)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_resolve_data_dir_explicit_absolute() {
        let path = Path::new("/custom/data/path");
        let resolved = resolve_data_dir(Some(path));
        assert_eq!(resolved, path);
    }

    #[test]
    fn test_resolve_data_dir_explicit_relative() {
        let path = Path::new("./my-data");
        let resolved = resolve_data_dir(Some(path));
        assert!(resolved.ends_with("my-data"));
    }

    #[test]
    fn test_resolve_data_dir_none_uses_system() {
        let resolved = resolve_data_dir(None);
        let path_str = resolved.to_string_lossy();
        assert!(
            path_str.contains("smppd") || path_str.ends_with("data"),
            "resolved path should contain smppd: {}",
            path_str
        );
    }

    #[tokio::test]
    async fn test_create_memory_storage() {
        let config = StoreConfig::memory();
        let storage = create_storage(&config).await.unwrap();

        use crate::store::StoredMessage;
        let msg = StoredMessage::new("SRC", "DST", b"test".to_vec(), "sys", 1);
        let id = storage.store(msg);
        assert!(storage.get(id).is_some());
    }
}
