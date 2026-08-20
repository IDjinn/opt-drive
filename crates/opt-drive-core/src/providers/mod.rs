//! Providers de backup/sync. Trait comum para múltiplos backends.
//!
//! As implementações concretas (S3, Google Drive, mirror local) vivem no crate
//! `opt-drive-connectors` — o core define o contrato e o motor incremental
//! ([`sync`]), sem conhecer rede/HTTP (invariante 5 do AGENTS.md).

pub mod sync;

use std::path::Path;

use serde::{Deserialize, Serialize};

use sync::{SyncContext, Transport};

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
    /// Nome do provider (ex.: `"s3"`).
    fn name(&self) -> &str;

    /// Autentica / valida credenciais.
    fn authenticate(&self) -> anyhow::Result<()>;

    /// Consulta o estado remoto de um caminho local.
    fn status(&self, local: &Path) -> anyhow::Result<RemoteStatus>;

    /// Sincroniza incrementalmente uma pasta (upload do que mudou).
    fn sync_dir(&self, local: &Path, ctx: &SyncContext) -> anyhow::Result<SyncReport>;

    /// Baixa um item remoto (`remote_id` do `backup_state`) de volta para `dst`.
    fn restore(&self, remote_id: &str, dst: &Path, ctx: &SyncContext) -> anyhow::Result<()>;

    /// Acesso ao transporte (usado pelo motor em [`sync`]).
    fn transport(&self) -> &dyn Transport;
}

/// Implementa [`BackupProvider`] para qualquer tipo que implemente
/// [`Transport`] — os conectores só escrevem as 4 primitivas.
pub struct ProviderAdapter<T: Transport> {
    pub name: String,
    pub inner: T,
    /// Prefixo no destino (ex.: `Opt-Drive/<pasta>`); vazio = raiz.
    pub prefix: String,
}

impl<T: Transport> ProviderAdapter<T> {
    /// Prefixo efetivo no destino: `<prefix>/<nome-da-pasta>` (evita colisão
    /// quando múltiplos paths são sincronizados com o mesmo conector).
    fn effective_prefix(&self, local: &Path) -> String {
        match local.file_name().and_then(|n| n.to_str()) {
            Some(name) if !self.prefix.is_empty() => format!("{}/{}", self.prefix, name),
            Some(name) => name.to_string(),
            None => self.prefix.clone(),
        }
    }
}

impl<T: Transport> BackupProvider for ProviderAdapter<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn authenticate(&self) -> anyhow::Result<()> {
        // Transportes sem credenciais (local) estão sempre autenticados.
        Ok(())
    }

    fn status(&self, local: &Path) -> anyhow::Result<RemoteStatus> {
        // Status por arquivo é consultado via remote_id derivado do caminho.
        let rel = Path::new(local.file_name().unwrap_or_default());
        let id = sync::remote_id_for(&self.effective_prefix(local), rel, false);
        if self.inner.exists(&id)? {
            let size = local.metadata().map(|m| m.len()).ok();
            Ok(RemoteStatus {
                exists: true,
                remote_id: Some(id),
                remote_mtime: None,
                remote_size: size,
            })
        } else {
            Ok(RemoteStatus {
                exists: false,
                remote_id: None,
                remote_mtime: None,
                remote_size: None,
            })
        }
    }

    fn sync_dir(&self, local: &Path, ctx: &SyncContext) -> anyhow::Result<SyncReport> {
        sync::sync_dir(&self.inner, local, &self.effective_prefix(local), ctx)
    }

    fn restore(&self, remote_id: &str, dst: &Path, ctx: &SyncContext) -> anyhow::Result<()> {
        sync::restore_file(&self.inner, remote_id, dst, ctx.passphrase)
    }

    fn transport(&self) -> &dyn Transport {
        &self.inner
    }
}
