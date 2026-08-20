//! Conectores de backup para o opt-drive (S3, Google Drive, mirror local).
//!
//! Implementações de rede vivem aqui (não no core) para manter o core testável
//! — invariante 5 do AGENTS.md. Todos expõem [`opt_drive_core::providers::BackupProvider`]
//! via [`opt_drive_core::providers::ProviderAdapter`].

pub mod google_drive;
pub mod local;
pub mod s3;
pub mod sigv4;

use anyhow::Context as _;
use opt_drive_core::config::BackupConfig;
use opt_drive_core::providers::{BackupProvider, ProviderAdapter};

/// Constrói o conector configurado em `[backup]`. `prefix` (raiz no destino) é
/// derivado do nome do conector: ex. `Opt-Drive`.
pub fn connector_from_config(cfg: &BackupConfig) -> anyhow::Result<Box<dyn BackupProvider>> {
    let prefix = "Opt-Drive".to_string();
    match cfg.connector.as_str() {
        "local" => {
            let root = cfg
                .options
                .get("target_root")
                .context("connector 'local' exige a opção 'target_root'")?;
            Ok(Box::new(ProviderAdapter {
                name: "local".into(),
                prefix,
                inner: local::LocalTransport::new(root),
            }))
        }
        "s3" => {
            let bucket = cfg
                .options
                .get("bucket")
                .context("connector 's3' exige a opção 'bucket'")?;
            Ok(Box::new(ProviderAdapter {
                name: "s3".into(),
                prefix,
                inner: s3::S3Transport::from_options(bucket, &cfg.options)?,
            }))
        }
        "google-drive" => {
            let creds = cfg
                .options
                .get("credentials_path")
                .context("connector 'google-drive' exige a opção 'credentials_path' (client_secret do Google Cloud Console)")?;
            Ok(Box::new(ProviderAdapter {
                name: "google-drive".into(),
                prefix,
                inner: google_drive::GoogleDriveTransport::new(creds),
            }))
        }
        other => anyhow::bail!("conector desconhecido: {other:?} (use \"local\", \"s3\" ou \"google-drive\")"),
    }
}
