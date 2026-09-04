//! Indexação incremental: aplica mudanças do filesystem (eventos de um file-watcher)
//! ao índice SQLite sem re-varrer tudo.
//!
//! O watcher (no daemon, via `notify`) coleta eventos e os empacota num [`ChangeBatch`].
//! O core então decide, para cada path, se ele deve ser **indexado** (upsert) ou
//! **ignorado** — aplicando exatamente as mesmas regras do scan completo
//! ([`crate::index::walker`]):
//! - `ignore_globs` casam contra o caminho ou o nome;
//! - arquivos "junk" (`*.log`, `.DS_Store`, …) NÃO são indexados;
//! - conteúdo dentro de subárvores de cleanup (`node_modules/…`, `target/…`) é
//!   ignorado; o próprio dir de cleanup é indexado como **marcador**.
//!
//! Isso mantém o índice consistente entre scan completo e atualizações incrementais.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use globset::GlobSet;
use serde::{Deserialize, Serialize};

use crate::cleanup_catalog::CleanupRules;

use super::walker::{build_globset, lossy_name};

/// Tipo de mudança observada no filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsChangeKind {
    /// Criação ou modificação → (re)indexar a entrada.
    Upsert,
    /// Remoção → apagar a entrada (e descendentes, se for diretório).
    Remove,
}

/// Uma mudança individual.
#[derive(Debug, Clone)]
pub struct FsChange {
    pub path: PathBuf,
    pub kind: FsChangeKind,
}

impl FsChange {
    pub fn upsert(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: FsChangeKind::Upsert,
        }
    }
    pub fn remove(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: FsChangeKind::Remove,
        }
    }
}

/// Lote coalescido de mudanças. Chaves por path com **last-write-wins**: se um mesmo
/// path aparece várias vezes num burst (ex.: create→modify→remove), prevalece o último
/// evento. Renames viram dois paths distintos (remove do antigo + upsert do novo).
#[derive(Debug, Default)]
pub struct ChangeBatch {
    map: HashMap<PathBuf, FsChangeKind>,
}

impl ChangeBatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adiciona uma mudança, sobrescrevendo o estado anterior do mesmo path.
    pub fn push(&mut self, path: PathBuf, kind: FsChangeKind) {
        self.map.insert(path, kind);
    }

    pub fn push_change(&mut self, change: FsChange) {
        self.map.insert(change.path, change.kind);
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Path, &FsChangeKind)> {
        self.map.iter().map(|(p, k)| (p.as_path(), k))
    }
}

/// Estatísticas de uma aplicação incremental.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChangeStats {
    pub upserted: usize,
    pub removed: usize,
    pub skipped: usize,
}

/// Decisão de indexação para um path.
pub(super) enum Decision {
    Upsert,
    Skip,
}

/// Classifica um path: indexar (upsert) ou ignorar? Espelha a lógica de poda do
/// walker, mas aplicada a um evento isolado.
pub(super) fn classify(
    path: &Path,
    is_dir: bool,
    ignore: Option<&GlobSet>,
    rules: &CleanupRules,
) -> Decision {
    let name = lossy_name(path);

    // ignore_globs casam contra o caminho OU o nome (igual ao walker).
    if let Some(set) = ignore {
        if set.is_match(path) || set.is_match(&name) {
            return Decision::Skip;
        }
    }

    if is_dir {
        // Dir de cleanup (node_modules, target, …): indexa como marcador, NÃO desce.
        if rules.matches_dir(&name) {
            return Decision::Upsert;
        }
        // Dentro de uma subárvore de cleanup? algum ancestral casa → ignora.
        if inside_cleanup_subtree(path, rules) {
            return Decision::Skip;
        }
        Decision::Upsert
    } else {
        // Arquivo "junk" (*.log, .DS_Store, *.pyc, …): não indexa.
        if rules.matches_file(&name) {
            return Decision::Skip;
        }
        if inside_cleanup_subtree(path, rules) {
            return Decision::Skip;
        }
        Decision::Upsert
    }
}

/// Algum componente ancestral (acima de `path`) é um diretório de cleanup?
/// Ex.: `C:\proj\node_modules\pkg\a.js` → `node_modules` casa → `true`.
fn inside_cleanup_subtree(path: &Path, rules: &CleanupRules) -> bool {
    let mut cur = path.parent();
    while let Some(dir) = cur {
        if let Some(name) = dir.file_name() {
            if rules.matches_dir(&name.to_string_lossy()) {
                return true;
            }
        }
        cur = dir.parent();
    }
    false
}

/// Compila `ignore_globs` num `GlobSet` (reutilizado por [`super::IndexDb::apply_changes`]).
pub(super) fn compile_ignore(globs: &[String]) -> Option<GlobSet> {
    build_globset(globs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup_catalog::CleanupTarget;

    fn rules() -> CleanupRules {
        CleanupRules::from_targets(&[
            CleanupTarget::dir("node_modules", "javascript"),
            CleanupTarget::files("log", &["*.log"], "logs"),
        ])
        .unwrap()
    }

    #[test]
    fn batch_last_write_wins() {
        let mut b = ChangeBatch::new();
        let p = Path::new("C:\\proj\\a.txt");
        b.push(p.into(), FsChangeKind::Upsert);
        b.push(p.into(), FsChangeKind::Remove);
        assert_eq!(b.len(), 1);
        assert_eq!(b.map.get(p), Some(&FsChangeKind::Remove));
    }

    #[test]
    fn batch_rename_is_two_paths() {
        let mut b = ChangeBatch::new();
        b.push(Path::new("C:\\old.txt").into(), FsChangeKind::Remove);
        b.push(Path::new("C:\\new.txt").into(), FsChangeKind::Upsert);
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn classify_indexes_normal_file_and_dir() {
        let r = rules();
        assert!(matches!(
            classify(Path::new("C:\\proj\\a.rs"), false, None, &r),
            Decision::Upsert
        ));
        assert!(matches!(
            classify(Path::new("C:\\proj\\src"), true, None, &r),
            Decision::Upsert
        ));
    }

    #[test]
    fn classify_skips_junk_file() {
        let r = rules();
        assert!(matches!(
            classify(Path::new("C:\\proj\\app.log"), false, None, &r),
            Decision::Skip
        ));
        // node_modules é dir → marcador (Upsert), não junk.
        assert!(matches!(
            classify(Path::new("C:\\proj\\node_modules"), true, None, &r),
            Decision::Upsert
        ));
    }

    #[test]
    fn classify_skips_inside_cleanup_subtree() {
        let r = rules();
        // filho de node_modules → ignora (mesmo sendo .rs).
        assert!(matches!(
            classify(
                Path::new("C:\\proj\\node_modules\\pkg\\a.js"),
                false,
                None,
                &r
            ),
            Decision::Skip
        ));
        // subdir de node_modules → ignora.
        assert!(matches!(
            classify(
                Path::new("C:\\proj\\node_modules\\pkg"),
                true,
                None,
                &r
            ),
            Decision::Skip
        ));
    }
}
