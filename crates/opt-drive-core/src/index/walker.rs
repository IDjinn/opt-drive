//! Varredura de diretórios com poda de dependências regeneráveis.
//!
//! Usa `walkdir` (DFS) e:
//! - **poda** subárvores de cleanup targets (`node_modules`, `target`, …): registra
//!   o diretório como marcador mas NÃO desce nele (evita indexar 100k+ arquivos);
//! - **ignora** paths que casam com `ignore_globs` (via `globset`);
//! - detecta **project roots** (diretório com `.git`) e propaga para os filhos.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use globset::{Glob, GlobSet, GlobSetBuilder};
use walkdir::WalkDir;

use crate::cleanup_catalog::CleanupRules;

use super::FileEntry;

/// Varre `root` chamando `f` para cada entrada indexável.
///
/// - poda subárvores de cleanup (dirs que casam com `rules`) — registra um marcador;
/// - **pula** arquivos "inúteis" (*.log, .DS_Store, *.pyc, …) — não os indexa.
pub fn for_each_entry<F>(
    root: &Path,
    ignore_globs: &[String],
    rules: &CleanupRules,
    mut f: F,
) where
    F: FnMut(FileEntry),
{
    let ignore_set = build_globset(ignore_globs).ok();
    let mut projects: HashSet<PathBuf> = HashSet::new();

    let mut it = WalkDir::new(root)
        .follow_links(false)
        .min_depth(0)
        .into_iter();

    while let Some(res) = it.next() {
        let dent = match res {
            Ok(d) => d,
            Err(_) => continue,
        };
        let path = dent.path();

        // Ignora por glob (e poda o subtree se for diretório).
        if let Some(set) = &ignore_set {
            if set.is_match(path) || set.is_match(lossy_name(path)) {
                if dent.file_type().is_dir() {
                    it.skip_current_dir();
                }
                continue;
            }
        }

        // Dirs de sistema na raiz do drive (C:\Windows, $RECYCLE.BIN, …) e
        // arquivos de sistema (pagefile.sys, …): nunca indexar.
        if crate::protected::skip_in_walk(path) {
            if dent.file_type().is_dir() {
                it.skip_current_dir();
            }
            continue;
        }

        // Detecção de project root: diretório com `.git`.
        let is_dir = dent.file_type().is_dir();
        if is_dir && path.join(".git").exists() {
            projects.insert(path.to_path_buf());
        }
        let project_root = find_project_root(path, &projects);

        // Pula arquivos "inúteis" (*.log, .DS_Store, …): não indexa.
        if !is_dir {
            if let Some(name) = dent.file_name().to_str() {
                if rules.matches_file(name) {
                    continue;
                }
            }
        }

        // Cleanup dir (node_modules, target, …): registra marcador e poda subárvore.
        if is_dir {
            if let Some(name) = dent.file_name().to_str() {
                if rules.matches_dir(name) {
                    if let Some(e) = entry_from(&dent, &project_root) {
                        f(e);
                    }
                    it.skip_current_dir();
                    continue;
                }
            }
        }

        if let Some(e) = entry_from(&dent, &project_root) {
            f(e);
        }
    }
}

fn find_project_root(path: &Path, cache: &HashSet<PathBuf>) -> Option<String> {
    // `ancestors()` retorna do mais próximo ao mais distante — a primeira entrada
    // presente no cache é a raiz de projeto mais próxima.
    for ancestor in path.ancestors() {
        if cache.contains(ancestor) {
            return Some(ancestor.to_string_lossy().into_owned());
        }
    }
    None
}

/// Raiz de projeto (dir que contém `.git`) mais próxima de `path`, olhando os
/// ancestrais a partir do próprio `path`. Usado pela indexação incremental (evento
/// isolado) — no scan completo usamos o cache `find_project_root` por performance.
pub(super) fn project_root_of(path: &Path) -> Option<String> {
    for ancestor in path.ancestors() {
        if ancestor.join(".git").exists() {
            return Some(ancestor.to_string_lossy().into_owned());
        }
    }
    None
}

fn entry_from(dent: &walkdir::DirEntry, project: &Option<String>) -> Option<FileEntry> {
    let meta = dent.metadata().ok()?;
    Some(build_entry(dent.path(), &meta, project))
}

/// Constrói uma `FileEntry` a partir de metadata já obtida. Compartilhado entre o
/// walker (scan completo) e a indexação incremental (watcher).
pub(super) fn build_entry(
    path: &Path,
    meta: &std::fs::Metadata,
    project: &Option<String>,
) -> FileEntry {
    FileEntry {
        path: path.to_string_lossy().into_owned(),
        is_dir: meta.is_dir(),
        size: meta.len(),
        mtime: to_unix(meta.modified()),
        atime: to_unix(meta.accessed()),
        drive: String::new(),
        project_root: project.clone(),
    }
}

pub(super) fn build_globset(globs: &[String]) -> anyhow::Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(Glob::new(g)?);
    }
    Ok(b.build()?)
}

pub(super) fn lossy_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn to_unix(t: std::io::Result<SystemTime>) -> i64 {
    match t {
        Ok(t) => t
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn prunes_cleanup_targets_and_detects_project() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // projeto com .git e node_modules
        fs::create_dir_all(root.join("proj")).unwrap();
        fs::create_dir_all(root.join("proj").join(".git")).unwrap();
        fs::write(root.join("proj").join(".git").join("HEAD"), "x").unwrap();
        fs::write(root.join("proj").join("main.js"), "x").unwrap();
        fs::create_dir_all(root.join("proj").join("node_modules").join("pkg")).unwrap();
        fs::write(
            root.join("proj").join("node_modules").join("pkg").join("a.js"),
            "x",
        )
        .unwrap();

        let mut entries = Vec::new();
        let rules = crate::cleanup_catalog::CleanupRules::from_targets(&[
            crate::cleanup_catalog::CleanupTarget::dir("node_modules", "javascript"),
        ])
        .unwrap();
        for_each_entry(root, &[], &rules, |e| entries.push(e));

        let paths: Vec<_> = entries.iter().map(|e| e.path.clone()).collect();
        assert!(paths.iter().any(|p| p.ends_with("main.js")));
        assert!(paths.iter().any(|p| p.ends_with("node_modules"))); // marcador registrado
        // não indexa conteúdo de node_modules
        assert!(!paths.iter().any(|p| p.contains("pkg") && p.contains("a.js")));

        let proj_entry = entries
            .iter()
            .find(|e| e.path.ends_with("main.js"))
            .unwrap();
        assert!(proj_entry.project_root.as_deref().unwrap().ends_with("proj"));
    }
}
