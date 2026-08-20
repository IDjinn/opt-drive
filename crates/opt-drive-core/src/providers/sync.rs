//! Motor de sync incremental genérico (Fase 2).
//!
//! O motor faz o trabalho comum a todos os conectores: varre a pasta, calcula
//! SHA-256 por arquivo, compara com o manifest `backup_state` no SQLite e só
//! re-envia o que mudou. O conector só implementa [`Transport`] (upload/download
//! primitivos). Opcionalmente encripta antes do upload e decripta no restore
//! (ver [`crate::ops::encrypt`]).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sha2::{Digest, Sha256};

use crate::index::IndexDb;

use super::SyncReport;

/// Primitivas de transporte que cada conector implementa (S3, Drive, local...).
/// `remote_id` é opaco para o motor — tipicamente uma chave/caminho no destino.
pub trait Transport: Send + Sync {
    /// Envia `local` para `remote_id`. Retorna os bytes transferidos.
    fn upload(&self, local: &Path, remote_id: &str) -> anyhow::Result<u64>;
    /// Baixa `remote_id` para `dst` (arquivo local temporário).
    fn download(&self, remote_id: &str, dst: &Path) -> anyhow::Result<()>;
    /// Verifica existência no destino.
    fn exists(&self, remote_id: &str) -> anyhow::Result<bool>;
    /// Apaga no destino (só usado com `delete_remote = true`).
    fn delete(&self, remote_id: &str) -> anyhow::Result<()>;
}

/// Contexto de uma execução de sync/restore.
pub struct SyncContext<'a> {
    /// Manifest incremental (tabela `backup_state`).
    pub db: &'a IndexDb,
    /// Apaga no destino o que sumiu localmente. Default conservador: `false`.
    pub delete_remote: bool,
    /// Passphrase: se `Some`, arquivos são encriptados no upload (`.odenc`) e
    /// decriptados no restore.
    pub passphrase: Option<&'a str>,
    /// Callback de progresso `(arquivo, fração 0.0-1.0)`.
    pub progress: &'a dyn Fn(&str, f64),
}

impl<'a> SyncContext<'a> {
    pub fn new(db: &'a IndexDb) -> Self {
        Self {
            db,
            delete_remote: false,
            passphrase: None,
            progress: &|_, _| {},
        }
    }
}

/// Extensão adicionada aos arquivos encriptados no destino.
pub const ENC_EXT: &str = ".odenc";

/// Monta o `remote_id` de um arquivo local relativo à raiz sincronizada:
/// `<prefix>/<rel>` com barras normais (S3/Drive não gostam de `\`), mais
/// `ENC_EXT` quando encriptado.
pub fn remote_id_for(prefix: &str, rel: &Path, encrypted: bool) -> String {
    let rel = rel.to_string_lossy().replace('\\', "/");
    let id = if prefix.is_empty() {
        rel
    } else {
        format!("{}/{}", prefix.trim_end_matches('/'), rel)
    };
    if encrypted {
        format!("{}{}", id, ENC_EXT)
    } else {
        id
    }
}

/// SHA-256 de um arquivo (hex).
fn sha256_file(path: &Path) -> anyhow::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    use std::io::Read;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hex: String = hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect();
    let size = std::fs::metadata(path)?.len();
    Ok((hex, size))
}

/// Sincroniza incrementalmente `local` para o destino via `transport`.
/// `prefix` é a raiz no destino (ex.: `Opt-Drive/<nome-da-pasta>`).
pub fn sync_dir(
    transport: &dyn Transport,
    local: &Path,
    prefix: &str,
    ctx: &SyncContext,
) -> anyhow::Result<SyncReport> {
    let files = collect_files(local)?;
    let total = files.len().max(1) as f64;
    let mut report = SyncReport::default();

    for (i, rel) in files.iter().enumerate() {
        let abs = local.join(rel);
        let name = abs.to_string_lossy().to_string();
        (ctx.progress)(&name, i as f64 / total);

        let upload = (|| -> anyhow::Result<bool> {
            let (sha256, size) = sha256_file(&abs)?;
            let mtime = mtime_of(&abs);
            // Só re-envia se mudou desde o último sync bem-sucedido.
            if let Some(prev) = ctx.db.get_backup_entry(&abs.to_string_lossy()) {
                if prev.sha256 == sha256 && prev.mtime == mtime {
                    return Ok(false);
                }
            }

            let encrypted = ctx.passphrase.is_some();
            let remote_id = remote_id_for(prefix, rel, encrypted);
            // Payload a enviar: original ou cópia encriptada em temp.
            let mut tmp_path: Option<PathBuf> = None;
            let payload: &Path = match ctx.passphrase {
                Some(pass) => {
                    let tmp = tempfile::Builder::new()
                        .suffix(ENC_EXT)
                        .tempfile()
                        .context("criar temp para encriptação")?;
                    let (f, path) = tmp.keep()?;
                    drop(f);
                    crate::ops::encrypt::encrypt_file(&abs, &path, pass)?;
                    tmp_path = Some(path);
                    tmp_path.as_deref().unwrap()
                }
                None => &abs,
            };

            let bytes = transport.upload(payload, &remote_id)?;
            if let Some(p) = &tmp_path {
                let _ = std::fs::remove_file(p);
            }
            report.uploaded += 1;
            report.bytes_transferred += bytes;

            ctx.db.upsert_backup_entry(&crate::index::BackupEntry {
                path: abs.to_string_lossy().into_owned(),
                sha256,
                size,
                mtime,
                remote_id,
                encrypted,
                synced_at: unix_now(),
            })?;
            Ok(true)
        })();

        match upload {
            Ok(true) => {}
            Ok(false) => report.skipped += 1,
            Err(e) => {
                tracing::warn!(target: "opt-drive.backup", path = %name, error = %e, "falha no upload");
                report.failed += 1;
            }
        }
    }
    (ctx.progress)("", 1.0);
    Ok(report)
}

/// Baixa `remote_id` do destino para `dst`, decriptando se necessário.
pub fn restore_file(
    transport: &dyn Transport,
    remote_id: &str,
    dst: &Path,
    passphrase: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match passphrase {
        Some(pass) => {
            let tmp = tempfile::Builder::new()
                .suffix(ENC_EXT)
                .tempfile()
                .context("criar temp para download")?;
            let (f, tmp_path) = tmp.keep()?;
            drop(f);
            transport.download(remote_id, &tmp_path)?;
            crate::ops::encrypt::decrypt_file(&tmp_path, dst, pass)?;
            let _ = std::fs::remove_file(&tmp_path);
        }
        None => transport.download(remote_id, dst)?,
    }
    Ok(())
}

fn collect_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let e = entry?;
        if e.file_type().is_file() {
            out.push(e.path().strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(out)
}

fn mtime_of(path: &Path) -> i64 {
    use std::time::UNIX_EPOCH;
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_id_uses_slashes_and_enc_ext() {
        assert_eq!(remote_id_for("Opt-Drive/proj", Path::new("sub\\a.txt"), false), "Opt-Drive/proj/sub/a.txt");
        assert_eq!(remote_id_for("Opt-Drive/proj", Path::new("a.txt"), true), "Opt-Drive/proj/a.txt.odenc");
        assert_eq!(remote_id_for("", Path::new("a.txt"), false), "a.txt");
    }
}
