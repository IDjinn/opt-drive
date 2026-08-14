//! Limpeza de dependências regeneráveis e arquivos "inúteis" (junk de OS/editor/logs).
//!
//! **Atenção:** esta operação é destrutiva e NÃO é desfazível — os dados são
//! regeneráveis (`npm install`, `cargo build`, …) ou descartáveis (.DS_Store, *.log).
//! Só é executada quando explicitamente habilitada pela regra (`cleanup_deps = true`).

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::cleanup_catalog::CleanupRules;

/// Coleta todos os caminhos (dentro de `root`) que casam com as regras de cleanup:
/// diretórios (node_modules, target, …) — sem descer neles — e arquivos (*.log,
/// .DS_Store, *.pyc, …).
pub fn collect(root: &Path, rules: &CleanupRules) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut it = WalkDir::new(root).into_iter();
    while let Some(res) = it.next() {
        let dent = match res {
            Ok(d) => d,
            Err(_) => continue,
        };
        let Some(name) = dent.file_name().to_str() else {
            continue;
        };
        if dent.file_type().is_dir() {
            if rules.matches_dir(name) {
                found.push(dent.path().to_path_buf());
                it.skip_current_dir();
            }
        } else if rules.matches_file(name) {
            found.push(dent.path().to_path_buf());
        }
    }
    found
}

/// Estima o tamanho total ocupado pelos caminhos (soma recursiva p/ dirs).
pub fn estimate_bytes(paths: &[PathBuf]) -> u64 {
    paths.iter().map(|p| size_of_path(p)).sum()
}

/// Remove os caminhos informados (dirs via `remove_dir_all`, arquivos via
/// `remove_file`) e retorna quantos bytes foram liberados (estimativa pré-remoção).
pub fn remove(paths: &[PathBuf]) -> u64 {
    let mut freed = 0u64;
    for p in paths {
        freed += size_of_path(p);
        let res = if p.is_dir() {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        };
        if let Err(e) = res {
            tracing::warn!(target = "opt-drive.cleanup", path = %p.display(), error = %e, "falha ao remover");
        }
    }
    freed
}

fn size_of_path(path: &Path) -> u64 {
    if path.is_file() {
        return std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0u64;
    for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
        if entry.file_type().is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup_catalog::CleanupTarget;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn collects_dirs_and_junk_files() {
        let d = tempdir().unwrap();
        let root = d.path();
        fs::create_dir_all(root.join("proj").join("node_modules").join("x")).unwrap();
        fs::write(
            root.join("proj").join("node_modules").join("x").join("a"),
            "hello",
        )
        .unwrap();
        fs::write(root.join("proj").join("app.log"), "log").unwrap();
        fs::write(root.join("proj").join(".DS_Store"), "ds").unwrap();
        fs::write(root.join("proj").join("main.rs"), "code").unwrap();

        let rules = CleanupRules::from_targets(&[
            CleanupTarget::dir("node_modules", "javascript"),
            CleanupTarget::files("logs", &["*.log"], "logs"),
            CleanupTarget::files("os", &[".DS_Store"], "os"),
        ])
        .unwrap();

        let found = collect(root, &rules);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"node_modules".to_string()));
        assert!(names.contains(&"app.log".to_string()));
        assert!(names.contains(&".DS_Store".to_string()));

        let freed = remove(&found);
        assert!(freed >= 5);
        assert!(!root.join("proj").join("node_modules").exists());
        assert!(!root.join("proj").join("app.log").exists());
        assert!(root.join("proj").join("main.rs").exists()); // código preservado
    }
}
