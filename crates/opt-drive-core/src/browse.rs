//! Navegação de diretórios (explorer) com tamanhos de pasta.
//!
//! A **estrutura** (filhos, tipo, mtime) vem sempre do `read_dir` em tempo real — então
//! o explorer é instantâneo e reflete o disco agora. O **tamanho de cada pasta** vem do
//! índice SQLite (`IndexDb::dir_size`) quando a pasta foi indexada; pastas não
//! indexadas ficam com tamanho desconhecido (a UI mostra "—" — rode a indexação para
//! preenchê-los). Arquivos sempre mostram o tamanho real do `metadata` (instantâneo).
//!
//! Não fazemos cálculo recursivo sob demanda na listagem: isso exigiria um `walkdir`
//! completo por pasta (catastrófico na raiz de um drive: `C:\Windows`, `Program Files`,
//! … travam por minutos). O fluxo correto é indexar primeiro (com progresso visível) e
//! então navegar.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::index::IndexDb;

/// Uma entrada de diretório/arquivo listada pelo explorer.
#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    /// Momento de modificação (segundos Unix).
    pub mtime: i64,
    /// Para pastas: o tamanho veio do índice (`true`) ou é desconhecido (`false`)?
    /// Para arquivos sempre `false` (tamanho veio do metadata ao vivo).
    pub indexed: bool,
    /// Entrada especial (sistema, cloud-sync, junction) que o opt-drive nunca
    /// modifica — a UI marca com 🔒.
    pub protected: bool,
}

/// Remove o prefixo verbatim que `canonicalize()` retorna no Windows:
/// `\\?\C:\dev` → `C:\dev` e `\\?\UNC\srv\share` → `\\srv\share`.
///
/// As chaves do índice são gravadas pelo walker no formato plain (vêm direto de
/// `watch.paths`), então sem esta normalização o lookup de `dir_size` nunca bate.
pub fn normalize_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{}", rest));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    p.to_path_buf()
}

/// Lista os filhos de `path`, ordenando diretórios antes de arquivos (alfabético,
/// case-insensitive). Instantâneo: só `read_dir` + lookup no índice.
///
/// Entradas inacessíveis (sem permissão, etc.) são silenciosamente ignoradas.
pub fn list_dir(path: &Path, db: Option<&IndexDb>) -> anyhow::Result<Vec<DirEntry>> {
    let rd = std::fs::read_dir(path)?;
    let mut entries = Vec::new();
    for child in rd.flatten() {
        let meta = match child.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = child.file_name().to_string_lossy().into_owned();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // file_type (sem seguir links) detecta junctions/symlinks — metadados
        // seguidos não distinguem um link do alvo.
        let is_reparse = child.file_type().map(|ft| ft.is_symlink()).unwrap_or(false);
        let protected =
            is_reparse || crate::protected::is_protected(&child.path()).is_some();

        let is_dir = meta.is_dir();
        let (size_bytes, indexed) = if is_dir {
            // Tamanho só do índice; se não houver, fica desconhecido (0 + indexed=false).
            let key = child.path().to_string_lossy().into_owned();
            match db.and_then(|d| d.dir_size(&key)) {
                Some(sz) => (sz, true),
                None => (0, false),
            }
        } else {
            (meta.len(), false)
        };

        entries.push(DirEntry {
            name,
            is_dir,
            size_bytes,
            mtime,
            indexed,
            protected,
        });
    }

    // Diretórios primeiro; dentro de cada grupo, alfabético case-insensitive.
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileEntry;
    use tempfile::tempdir;

    fn file(path: &str, size: u64) -> FileEntry {
        FileEntry {
            path: path.into(),
            is_dir: false,
            size,
            mtime: 0,
            atime: 0,
            drive: "C:\\".into(),
            project_root: None,
        }
    }

    #[test]
    fn list_dir_uses_index_for_folders() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        // Cria estrutura: proj/sub/b.txt (200), proj/a.txt (100), solto.txt (50).
        std::fs::create_dir_all(root.join("proj").join("sub")).unwrap();
        std::fs::write(root.join("proj").join("a.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(root.join("proj").join("sub").join("b.txt"), vec![0u8; 200]).unwrap();
        std::fs::write(root.join("solto.txt"), vec![0u8; 50]).unwrap();

        // Índice conhece "proj" (300 no total).
        let db = IndexDb::open(&root.join("idx.db")).unwrap();
        let proj = root.join("proj").to_string_lossy().replace('/', "\\");
        db.upsert_many(&[
            file(&format!("{proj}\\a.txt"), 100),
            file(&format!("{proj}\\sub\\b.txt"), 200),
        ])
        .unwrap();

        let entries = list_dir(root, Some(&db)).unwrap();
        // Diretório vem antes do arquivo.
        let proj_entry = entries.iter().find(|e| e.name == "proj").unwrap();
        assert!(proj_entry.is_dir);
        assert_eq!(proj_entry.size_bytes, 300);
        assert!(proj_entry.indexed);

        let solto = entries.iter().find(|e| e.name == "solto.txt").unwrap();
        assert!(!solto.is_dir);
        assert_eq!(solto.size_bytes, 50);
        assert!(!solto.indexed);
    }

    #[test]
    fn list_dir_shows_unknown_for_unindexed_folder() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("nao_indexado")).unwrap();
        std::fs::write(root.join("nao_indexado").join("x.txt"), vec![0u8; 42]).unwrap();

        // Sem DB: a pasta não tem tamanho conhecido (0 + indexed=false); o arquivo sim.
        let entries = list_dir(root, None).unwrap();
        let dir = entries.iter().find(|e| e.name == "nao_indexado").unwrap();
        assert!(dir.is_dir);
        assert_eq!(dir.size_bytes, 0);
        assert!(!dir.indexed);
    }

    #[test]
    fn normalize_verbatim_strips_prefixes() {
        use super::normalize_verbatim;
        assert_eq!(
            normalize_verbatim(Path::new(r"\\?\C:\dev\proj")),
            PathBuf::from(r"C:\dev\proj")
        );
        assert_eq!(
            normalize_verbatim(Path::new(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
        // Path plain fica igual (idempotente).
        assert_eq!(
            normalize_verbatim(Path::new(r"C:\dev")),
            PathBuf::from(r"C:\dev")
        );
    }
}

