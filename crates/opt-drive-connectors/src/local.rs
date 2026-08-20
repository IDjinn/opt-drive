//! Espelhamento local entre drives (backup sem rede).
//!
//! Copia os arquivos para `target_root` preservando a estrutura. Útil como
//! backup offline (ex.: SSD → HD) e como referência/teste do motor de sync.

use std::path::{Path, PathBuf};

use opt_drive_core::providers::sync::Transport;

/// Transporte que grava em uma pasta local, tratando `remote_id` como caminho
/// relativo à raiz (com `/` como separador).
pub struct LocalTransport {
    root: PathBuf,
}

impl LocalTransport {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve um `remote_id` (`a/b/c.txt`) para caminho local no mirror.
    fn path_of(&self, remote_id: &str) -> PathBuf {
        // remote_id usa `/`; converte para o separador do SO.
        let mut p = self.root.clone();
        for part in remote_id.split('/') {
            p.push(part);
        }
        p
    }
}

impl Transport for LocalTransport {
    fn upload(&self, local: &Path, remote_id: &str) -> anyhow::Result<u64> {
        let dst = self.path_of(remote_id);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Substitui versão anterior (não há versionamento local).
        if dst.exists() {
            std::fs::remove_file(&dst)?;
        }
        std::fs::copy(local, &dst)?;
        Ok(std::fs::metadata(&dst)?.len())
    }

    fn download(&self, remote_id: &str, dst: &Path) -> anyhow::Result<()> {
        let src = self.path_of(remote_id);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
        Ok(())
    }

    fn exists(&self, remote_id: &str) -> anyhow::Result<bool> {
        Ok(self.path_of(remote_id).exists())
    }

    fn delete(&self, remote_id: &str) -> anyhow::Result<()> {
        let p = self.path_of(remote_id);
        if p.exists() {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opt_drive_core::index::IndexDb;
    use opt_drive_core::providers::sync::SyncContext;

    fn write(root: &Path, rel: &str, content: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn mirror_roundtrip_and_incremental() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        write(&src, "a.txt", b"conteudo A");
        write(&src, "sub\\b.txt", b"conteudo B");

        let mirror = tmp.path().join("mirror");
        let db = IndexDb::open(&tmp.path().join("idx.db")).unwrap();
        let t = LocalTransport::new(&mirror);
        let ctx = SyncContext::new(&db);

        // 1º sync: envia os 2 arquivos.
        let r1 = opt_drive_core::providers::sync::sync_dir(&t, &src, "Opt-Drive/src", &ctx).unwrap();
        assert_eq!(r1.uploaded, 2);
        assert_eq!(r1.failed, 0);
        assert!(mirror.join("Opt-Drive").join("src").join("a.txt").exists());

        // 2º sync sem mudanças: tudo skipped.
        let r2 = opt_drive_core::providers::sync::sync_dir(&t, &src, "Opt-Drive/src", &ctx).unwrap();
        assert_eq!(r2.uploaded, 0);
        assert_eq!(r2.skipped, 2);

        // Muda um arquivo: só ele é re-enviado.
        write(&src, "a.txt", b"conteudo A v2");
        let r3 = opt_drive_core::providers::sync::sync_dir(&t, &src, "Opt-Drive/src", &ctx).unwrap();
        assert_eq!(r3.uploaded, 1);
        assert_eq!(r3.skipped, 1);

        // Restore volta o conteúdo mais recente.
        let dst = tmp.path().join("restored").join("a.txt");
        opt_drive_core::providers::sync::restore_file(&t, "Opt-Drive/src/a.txt", &dst, None)
            .unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"conteudo A v2");
    }

    #[test]
    fn encrypted_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        write(&src, "a.txt", b"segredo");
        let mirror = tmp.path().join("mirror");
        let db = IndexDb::open(&tmp.path().join("idx.db")).unwrap();
        let t = LocalTransport::new(&mirror);
        let ctx = SyncContext {
            db: &db,
            delete_remote: false,
            passphrase: Some("senha"),
            progress: &|_, _| {},
        };

        let r = opt_drive_core::providers::sync::sync_dir(&t, &src, "bkp", &ctx).unwrap();
        assert_eq!(r.uploaded, 1);
        // No mirror o arquivo é .odenc e não contém o plaintext.
        let raw = std::fs::read(mirror.join("bkp").join("a.txt.odenc")).unwrap();
        assert!(!raw.windows(6).any(|w| w == b"segredo"));

        // Restore decripta de volta.
        let dst = tmp.path().join("out").join("a.txt");
        opt_drive_core::providers::sync::restore_file(&t, "bkp/a.txt.odenc", &dst, Some("senha"))
            .unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"segredo");
    }
}
