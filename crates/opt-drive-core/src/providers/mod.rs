//! Providers de backup/sync. Trait comum para múltiplos backends.
//!
//! A Fase 2 implementa o `GoogleDrive`. Outros (OneDrive, Dropbox, REST genérico)
//! podem ser adicionados implementando [`BackupProvider`].

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Status de um item no provedor remoto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteStatus {
    pub exists: bool,
    pub remote_id: Option<String>,
    pub remote_mtime: Option<i64>,
    pub remote_size: Option<u64>,
}

/// Resultado de um sync incremental.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncReport {
    pub uploaded: u64,
    pub skipped: u64,
    pub failed: u64,
    pub bytes_transferred: u64,
}

/// Trait comum a todos os backends de backup.
pub trait BackupProvider: Send + Sync {
    /// Nome do provider (ex.: `"google-drive"`).
    fn name(&self) -> &str;

    /// Autentica / valida credenciais.
    fn authenticate(&self) -> anyhow::Result<()>;

    /// Consulta o estado remoto de um caminho local.
    fn status(&self, local: &Path) -> anyhow::Result<RemoteStatus>;

    /// Sincroniza incrementalmente uma pasta (upload do que mudou).
    fn sync_dir(&self, local: &Path) -> anyhow::Result<SyncReport>;
}

/// (Fase 2) Provider Google Drive — placeholder até a implementação OAuth.
pub mod google_drive {
    use super::{BackupProvider, RemoteStatus, SyncReport};
    use std::path::Path;

    /// Implementação pendente (Fase 2): auth OAuth2, upload resumível, sync por hash/mtime.
    pub struct GoogleDrive {
        #[allow(dead_code)]
        credentials_path: std::path::PathBuf,
    }

    impl GoogleDrive {
        pub fn new(credentials_path: impl AsRef<Path>) -> Self {
            Self {
                credentials_path: credentials_path.as_ref().to_path_buf(),
            }
        }
    }

    impl BackupProvider for GoogleDrive {
        fn name(&self) -> &str {
            "google-drive"
        }
        fn authenticate(&self) -> anyhow::Result<()> {
            anyhow::bail!("Google Drive: implementação na Fase 2")
        }
        fn status(&self, _local: &Path) -> anyhow::Result<RemoteStatus> {
            anyhow::bail!("Google Drive: implementação na Fase 2")
        }
        fn sync_dir(&self, _local: &Path) -> anyhow::Result<SyncReport> {
            anyhow::bail!("Google Drive: implementação na Fase 2")
        }
    }
}
