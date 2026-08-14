//! Cálculo de "atividade" de um arquivo/projeto.
//!
//! O NTFS desabilita a atualização de *last-access* por padrão desde o Vista, então
//! não podemos depender só de `atime`. Combinamos:
//! - `mtime` (sempre confiável),
//! - `atime` (quando presente),
//! - data do **último commit git** do projeto (sinal forte de "ativo").
//!
//! O resultado é o maior (mais recente) entre esses sinais.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use crate::index::FileEntry;

#[derive(Default)]
pub struct ActivityScorer {
    /// Cache de "último commit git" por raiz de projeto.
    git_cache: Mutex<HashMap<PathBuf, Option<i64>>>,
    /// Desabilita a invocação do git quando `false`.
    use_git: bool,
}

impl ActivityScorer {
    pub fn new() -> Self {
        Self {
            git_cache: Mutex::new(HashMap::new()),
            use_git: true,
        }
    }

    pub fn with_git(self, use_git: bool) -> Self {
        Self { use_git, ..self }
    }

    /// Maior timestamp de atividade conhecido para a entrada (segundos Unix).
    ///
    /// Usamos **mtime** (confiável) + data do **último commit git**. O `atime` é
    /// propositalmente ignorado aqui: no NTFS o last-access é desabilitado por padrão
    /// e, quando habilitado, é atualizado por qualquer leitura (inclusive a do próprio
    /// scan), tornando-o um sinal não confiável. O tracking real de "acesso" (abrir
    /// arquivos) virá com o file-watcher do daemon.
    pub fn last_activity(&self, entry: &FileEntry) -> i64 {
        let mut best = entry.mtime;
        if let Some(root) = &entry.project_root {
            if let Some(t) = self.git_commit_time(Path::new(root)) {
                best = best.max(t);
            }
        }
        best
    }

    /// Dias desde a última atividade (>= 0).
    pub fn days_inactive(&self, entry: &FileEntry, now: i64) -> i64 {
        ((now - self.last_activity(entry)) / 86_400).max(0)
    }

    fn git_commit_time(&self, root: &Path) -> Option<i64> {
        if !self.use_git {
            return None;
        }
        if let Ok(cache) = self.git_cache.lock() {
            if let Some(v) = cache.get(root) {
                return *v;
            }
        }

        let value = read_git_last_commit(root);
        if let Ok(mut cache) = self.git_cache.lock() {
            cache.insert(root.to_path_buf(), value);
        }
        value
    }
}

/// Executa `git -C <root> log -1 --format=%ct` e converte para segundos Unix.
fn read_git_last_commit(root: &Path) -> Option<i64> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-1", "--format=%ct"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    s.parse::<i64>().ok()
}

/// Tempo Unix atual (segundos).
pub fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(mtime: i64, atime: i64) -> FileEntry {
        FileEntry {
            path: "x".into(),
            is_dir: false,
            size: 0,
            mtime,
            atime,
            drive: String::new(),
            project_root: None,
        }
    }

    #[test]
    fn uses_mtime_ignoring_atime() {
        let s = ActivityScorer::new();
        // atime (200) maior que mtime (100): é ignorado — last_activity = mtime.
        assert_eq!(s.last_activity(&entry(100, 200)), 100);
        assert_eq!(s.last_activity(&entry(300, 0)), 300);
    }

    #[test]
    fn days_inactive_clamped() {
        let s = ActivityScorer::new();
        // atividade no futuro (clock skew) → 0 dias
        assert_eq!(s.days_inactive(&entry(unix_now() + 100_000, 0), unix_now()), 0);
    }
}
