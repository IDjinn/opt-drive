//! Opt-Drive core: lógica de domínio para gerenciamento de arquivos e backups.
//!
//! Módulos principais:
//! - [`config`]: modelo de configuração (TOML).
//! - [`drives`]: enumeração e classificação de drives (NVMe/SSD/HDD → tier).
//! - [`index`]: varredura do filesystem e persistência em SQLite.
//! - [`browse`]: listagem de diretórios (explorer) com tamanhos de pasta.
//! - [`usage`]: cálculo de "atividade" (combina mtime/atime/git).
//! - [`policy`]: engine de regras → lista de [`policy::Action`].
//! - [`ops`]: execução de operações (mover+junction, limpeza, compressão) com journal/undo.
//! - [`providers`]: trait [`providers::BackupProvider`] (Google Drive na fase 2).

pub mod browse;
pub mod cleanup_catalog;
pub mod config;
pub mod drives;
pub mod index;
pub mod ops;
pub mod policy;
pub mod providers;
pub mod usage;

pub use browse::DirEntry;
pub use cleanup_catalog::{CleanupEntry, CleanupRules, CleanupTarget};
pub use config::Config;
pub use drives::{Drive, DriveKind, Tier};
pub use index::FileEntry;
pub use policy::{Action, Rule};

/// Caminho do arquivo de config do usuário (criado sob o diretório de config do SO).
pub fn default_config_path() -> std::path::PathBuf {
    let proj = directories::ProjectDirs::from("dev", "optdrive", "opt-drive")
        .expect("não foi possível resolver o diretório de config do SO");
    proj.config_dir().join("config.toml")
}

/// Caminho do banco SQLite de índice (diretório de dados do SO).
pub fn default_db_path() -> std::path::PathBuf {
    let proj = directories::ProjectDirs::from("dev", "optdrive", "opt-drive")
        .expect("não foi possível resolver o diretório de dados do SO");
    let dir = proj.data_dir();
    let _ = std::fs::create_dir_all(dir);
    dir.join("index.sqlite")
}
