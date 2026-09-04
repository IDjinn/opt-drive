//! Varredura do filesystem e persistência do índice em SQLite.

pub mod db;
pub mod walker;
pub mod watch;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use ignore::{WalkBuilder, WalkState};
use serde::{Deserialize, Serialize};

use crate::cleanup_catalog::{CleanupRules, CleanupTarget};
use crate::drives::mount_of;

pub use db::{BackupEntry, IndexDb};
pub use watch::{ChangeBatch, ChangeStats, FsChange, FsChangeKind};

/// Uma entrada indexada (arquivo ou diretório).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Tempo de modificação (segundos Unix).
    pub mtime: i64,
    /// Tempo de acesso (segundos Unix; pode ser 0 se indisponível).
    pub atime: i64,
    /// Ponto de montagem do drive que contém o arquivo.
    pub drive: String,
    /// Diretório raiz do projeto (que contém `.git`), se aplicável.
    pub project_root: Option<String>,
}

/// Estatísticas de uma execução de scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanStats {
    pub roots_scanned: usize,
    pub entries_indexed: usize,
    pub dirs: usize,
    pub files: usize,
    pub total_bytes: u64,
    pub errors: usize,
}

/// Progresso parcial de uma varredura — emitido periodicamente durante o scan (o
/// chamador deriva %, velocidade e ETA a partir destes contadores brutos).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanProgress {
    /// Entradas indexadas até o momento.
    pub indexed: usize,
    /// Pasta na "ponta" do scan (leading edge). Pode variar entre threads; é só informativa.
    pub current_dir: Option<String>,
    /// Estimativa do total de entradas (baseada no último scan completo). `None` no 1º scan.
    pub total_estimate: Option<usize>,
    /// Tempo decorrido desde o início do scan (ms).
    pub elapsed_ms: u64,
    /// Bytes acumulados (somatório de `size` dos arquivos vistos).
    pub bytes: u64,
    /// Erros de I/O/metadados até o momento.
    pub errors: usize,
}

/// Indexação em si (varredura e incremental). São métodos de [`IndexDb`] — e não de
/// um wrapper — para que o daemon compartilhe **uma única conexão de escrita** entre
/// watcher, scans e backups: em WAL o SQLite só permite um writer por vez, e
/// conexões concorrentes disputam o lock do banco ("database is locked"). Numa
/// instância só, os writers fazem fila no `Mutex` da aplicação — sempre succeed, sem
/// timeout. Leituras podem usar conexões separadas: no WAL leitores não bloqueiam.
impl IndexDb {
    /// Varre as raízes **em paralelo** (`ignore::WalkParallel`, o motor do ripgrep) e
    /// grava no banco em lotes. `progress` é chamado periodicamente com um
    /// [`ScanProgress`] parcial — entradas, pasta atual, bytes, elapsed — para a UI/CLI
    /// derivarem %, velocidade e ETA.
    ///
    /// - `threads`: nº de threads de varredura (`0` = automático = nº de CPUs). Limita o
    ///   uso de CPU durante o scan.
    /// - `cancel`: flag opcional; se setado (`true`) durante a varredura, o scan para
    ///   o quanto antes e devolve estatísticas parciais.
    ///
    /// Mantém a semântica do walker sequencial: poda subárvores de cleanup
    /// (`node_modules`, `target`, …) como marcadores, pula arquivos "inúteis" (*.log,
    /// .DS_Store, …) e **não** respeita `.gitignore`/hidden (`.standard_filters(false)`).
    pub fn scan<F>(
        &self,
        roots: &[PathBuf],
        ignore_globs: &[String],
        cleanup_targets: &[CleanupTarget],
        threads: usize,
        cancel: Option<Arc<AtomicBool>>,
        mut progress: F,
    ) -> anyhow::Result<ScanStats>
    where
        F: FnMut(&ScanProgress) + Send,
    {
        if roots.is_empty() {
            let _ = self.touch_indexed();
            return Ok(ScanStats::default());
        }

        let n_threads = crate::config::resolve_threads(threads).max(1);
        let rules = Arc::new(
            CleanupRules::from_targets(cleanup_targets)
                .unwrap_or_else(|_| CleanupRules::empty()),
        );
        let ignore_set = Arc::new(walker::build_globset(ignore_globs).ok());

        // Drive por raiz, pré-computado UMA vez (mount_of faz canonicalize = syscall).
        // O root é mantido exatamente como entregue ao walker, p/ casar via `starts_with`.
        let roots_drive: Arc<Vec<(PathBuf, String)>> = Arc::new(
            roots
                .iter()
                .map(|r| (r.clone(), mount_of(r).unwrap_or_default()))
                .collect(),
        );
        let roots_len = roots_drive.len();

        // Estado compartilhado entre workers (varredura) e collector (gravação).
        let projects: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let current_dir: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let errors = Arc::new(AtomicUsize::new(0));
        let total_estimate = self.scan_total_estimate();
        let start = Instant::now();

        let (entry_tx, entry_rx) = mpsc::channel::<FileEntry>();
        let db = self;

        let stats = std::thread::scope(|s| -> anyhow::Result<ScanStats> {
            // --- Collector (thread com escopo): drena o canal, grava no SQLite em lotes,
            //     acumula stats e emite progresso (throttle por lote + por tempo). ---
            let cur_c = current_dir.clone();
            let err_c = errors.clone();
            let collector = s.spawn(move || -> ScanStats {
                let mut stats = ScanStats {
                    roots_scanned: roots_len,
                    ..Default::default()
                };
                let mut batch: Vec<FileEntry> = Vec::with_capacity(Self::BATCH);
                let mut bytes = 0u64;
                let mut last_emit = Instant::now();

                // Constrói um snapshot de progresso a partir dos contadores correntes.
                let mkprog = |indexed: usize, bytes: u64| -> ScanProgress {
                    let cur = cur_c.lock().ok().and_then(|g| {
                        let s = g.clone();
                        (!s.is_empty()).then_some(s)
                    });
                    ScanProgress {
                        indexed,
                        current_dir: cur,
                        total_estimate,
                        elapsed_ms: start.elapsed().as_millis() as u64,
                        bytes,
                        errors: err_c.load(Ordering::Relaxed),
                    }
                };

                loop {
                    match entry_rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(entry) => {
                            if entry.is_dir {
                                stats.dirs += 1;
                            } else {
                                stats.files += 1;
                                bytes += entry.size;
                            }
                            batch.push(entry);
                            stats.entries_indexed += 1;
                            if batch.len() >= Self::BATCH {
                                if let Err(e) = db.upsert_many(&batch) {
                                    err_c.fetch_add(1, Ordering::Relaxed);
                                    tracing::warn!(
                                        target: "opt-drive.index",
                                        error = %e,
                                        "upsert_many falhou"
                                    );
                                }
                                batch.clear();
                                if last_emit.elapsed() >= Duration::from_millis(200) {
                                    last_emit = Instant::now();
                                    progress(&mkprog(stats.entries_indexed, bytes));
                                }
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            // Emissão periódica mesmo sem lote cheio → feed suave em árvores
                            // pequenas/lentas.
                            if last_emit.elapsed() >= Duration::from_millis(200) {
                                last_emit = Instant::now();
                                progress(&mkprog(stats.entries_indexed, bytes));
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            // Workers terminaram: flush final + último progresso.
                            if !batch.is_empty() {
                                if let Err(e) = db.upsert_many(&batch) {
                                    err_c.fetch_add(1, Ordering::Relaxed);
                                    tracing::warn!(
                                        target: "opt-drive.index",
                                        error = %e,
                                        "upsert_many (final) falhou"
                                    );
                                }
                            }
                            progress(&mkprog(stats.entries_indexed, bytes));
                            break;
                        }
                    }
                }
                stats.errors = err_c.load(Ordering::Relaxed);
                stats.total_bytes = bytes;
                stats
            });

            // --- Walker paralelo: cada worker classifica/constroi a entry e envia ao collector. ---
            let mut builder = WalkBuilder::new(&roots_drive[0].0);
            for (r, _) in roots_drive.iter().skip(1) {
                builder.add(r);
            }
            builder.follow_links(false);
            builder.standard_filters(false); // não respeitar .gitignore/.ignore/hidden
            builder.threads(n_threads);

            // Clone dedicado ao walker (será movido para o closure `run`); o original
            // `entry_tx` é droppado após o run p/ fechar o canal e o collector finalizar.
            let run_tx = entry_tx.clone();
            builder
                .build_parallel()
                .run(move || {
                    // Clones por thread (make-worker é chamado 1x por worker).
                    let tx = run_tx.clone();
                    let rules = rules.clone();
                    let ignore_set = ignore_set.clone();
                    let roots_drive = roots_drive.clone();
                    let projects = projects.clone();
                    let current_dir = current_dir.clone();
                    let errors = errors.clone();
                    let cancel = cancel.clone();
                    Box::new(move |res| -> WalkState {
                        if let Some(c) = &cancel {
                            if c.load(Ordering::Relaxed) {
                                return WalkState::Quit;
                            }
                        }
                        let dent = match res {
                            Ok(d) => d,
                            Err(_) => {
                                errors.fetch_add(1, Ordering::Relaxed);
                                return WalkState::Continue;
                            }
                        };
                        let path = dent.path();
                        let is_dir = dent.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

                        // Ignora por glob (caminho ou nome). Poda o subtree se for diretório.
                        if let Some(set) = &*ignore_set {
                            if set.is_match(path) || set.is_match(walker::lossy_name(path)) {
                                return if is_dir {
                                    WalkState::Skip
                                } else {
                                    WalkState::Continue
                                };
                            }
                        }

                        // Dirs de sistema na raiz do drive (C:\Windows, …) e
                        // arquivos de sistema (pagefile.sys, …): nunca indexar.
                        if crate::protected::skip_in_walk(path) {
                            return if is_dir {
                                WalkState::Skip
                            } else {
                                WalkState::Continue
                            };
                        }

                        // Detecção de project root (dir com .git) → alimenta cache compartilhado.
                        if is_dir && path.join(".git").exists() {
                            if let Ok(mut p) = projects.lock() {
                                p.insert(path.to_path_buf());
                            }
                        }
                        let project = project_for(path, &projects);

                        // Atualiza a "ponta" do scan (informativo) ao visitar diretórios.
                        if is_dir {
                            if let Ok(mut g) = current_dir.lock() {
                                *g = path.to_string_lossy().into_owned();
                            }
                        }

                        // Pula arquivos "inúteis" (*.log, .DS_Store, …).
                        if !is_dir {
                            if let Some(name) = dent.file_name().to_str() {
                                if rules.matches_file(name) {
                                    return WalkState::Continue;
                                }
                            }
                        }

                        // Cleanup dir (node_modules, target, …): emite o marcador e poda a subárvore.
                        if is_dir {
                            if let Some(name) = dent.file_name().to_str() {
                                if rules.matches_dir(name) {
                                    if let Ok(meta) = dent.metadata() {
                                        let mut e = walker::build_entry(path, &meta, &project);
                                        e.drive = drive_of(path, &roots_drive);
                                        let _ = tx.send(e);
                                    }
                                    return WalkState::Skip;
                                }
                            }
                        }

                        match dent.metadata() {
                            Ok(meta) => {
                                let mut e = walker::build_entry(path, &meta, &project);
                                e.drive = drive_of(path, &roots_drive);
                                let _ = tx.send(e);
                                WalkState::Continue
                            }
                            Err(_) => {
                                errors.fetch_add(1, Ordering::Relaxed);
                                WalkState::Continue
                            }
                        }
                    })
                });

            drop(entry_tx); // workers terminaram → fecha o canal → collector faz flush final
            Ok(collector.join().expect("collector panic"))
        })?;

        // Marca o timestamp da indexação e grava a estimativa de total p/ o próximo scan/ETA.
        let _ = self.touch_indexed();
        let _ = self.set_scan_total(stats.entries_indexed);
        Ok(stats)
    }

    const BATCH: usize = 2000;

    /// Aplica um lote de mudanças incrementais (de um file-watcher) ao índice, numa
    /// única transação. Filtra exatamente como o scan completo (`ignore_globs`,
    /// arquivos junk, subárvores de cleanup). Retorna [`ChangeStats`].
    ///
    /// Mudanças cujo alvo desapareceu entre o evento e o processamento viram remoção
    /// (caso seguro em que o arquivo foi criado e apagado dentro da janela de debounce).
    pub fn apply_changes(
        &self,
        batch: &watch::ChangeBatch,
        ignore_globs: &[String],
        rules: &CleanupRules,
    ) -> anyhow::Result<watch::ChangeStats> {
        use watch::{classify, Decision};
        let ignore = watch::compile_ignore(ignore_globs);

        let mut upserts: Vec<FileEntry> = Vec::new();
        let mut removes: Vec<String> = Vec::new();
        let mut stats = watch::ChangeStats::default();

        for (path, kind) in batch.iter() {
            match kind {
                watch::FsChangeKind::Remove => {
                    removes.push(path.to_string_lossy().into_owned());
                }
                watch::FsChangeKind::Upsert => {
                    let meta = match std::fs::symlink_metadata(path) {
                        Ok(m) => m,
                        Err(_) => {
                            // Sumiu entre o evento e agora — trata como remoção.
                            removes.push(path.to_string_lossy().into_owned());
                            continue;
                        }
                    };
                    let is_dir = meta.is_dir();
                    match classify(path, is_dir, ignore.as_ref(), rules) {
                        Decision::Skip => stats.skipped += 1,
                        Decision::Upsert => {
                            let project = walker::project_root_of(path);
                            upserts.push(walker::build_entry(path, &meta, &project));
                        }
                    }
                }
            }
        }

        let n_up = upserts.len();
        let n_rm = removes.len();
        self.apply_mixed(&upserts, &removes)?;
        stats.upserted = n_up;
        stats.removed = n_rm;
        Ok(stats)
    }
}

/// Drive (ex.: `"C:\\"`) de um path, casando contra as raízes pré-computadas por
/// prefixo de componentes. Sem `canonicalize` → sem syscall por entrada (importante em
/// hot path de indexação).
fn drive_of(path: &Path, roots_drive: &[(PathBuf, String)]) -> String {
    for (root, drive) in roots_drive {
        if path.starts_with(root) {
            return drive.clone();
        }
    }
    String::new()
}

/// Project root do `path`: busca ancestrais no cache compartilhado (rápido, sem I/O);
/// se não achar, cai pra varredura no filesystem (`walker::project_root_of`) —
/// autoritativa, cobre o caso de uma entry ser processada antes do ancestral `.git`
/// ser descoberto por outra thread.
fn project_for(path: &Path, projects: &Mutex<HashSet<PathBuf>>) -> Option<String> {
    if let Ok(set) = projects.lock() {
        for ancestor in path.ancestors() {
            if set.contains(ancestor) {
                return Some(ancestor.to_string_lossy().into_owned());
            }
        }
    }
    walker::project_root_of(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup_catalog::CleanupTarget;
    use std::fs;
    use tempfile::tempdir;

    fn rules() -> CleanupRules {
        CleanupRules::from_targets(&[
            CleanupTarget::dir("node_modules", "javascript"),
            CleanupTarget::files("log", &["*.log"], "logs"),
        ])
        .unwrap()
    }

    #[test]
    fn apply_changes_inserts_new_file_and_dir() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("proj")).unwrap();
        fs::write(root.join("proj").join("a.rs"), "x").unwrap();

        let idx = IndexDb::open(&root.join("idx.db")).unwrap();
        let mut batch = ChangeBatch::new();
        batch.push_change(FsChange::upsert(root.join("proj")));
        batch.push_change(FsChange::upsert(root.join("proj").join("a.rs")));

        let stats = idx.apply_changes(&batch, &[], &rules()).unwrap();
        assert_eq!(stats.upserted, 2);
        assert_eq!(stats.skipped, 0);
        assert_eq!(idx.count(), 2);
    }

    #[test]
    fn apply_changes_skips_junk_and_cleanup_children() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("proj").join("node_modules").join("pkg")).unwrap();
        fs::write(root.join("proj").join("app.log"), "x").unwrap();
        fs::write(
            root.join("proj")
                .join("node_modules")
                .join("pkg")
                .join("a.js"),
            "x",
        )
        .unwrap();

        let idx = IndexDb::open(&root.join("idx.db")).unwrap();
        let mut batch = ChangeBatch::new();
        batch.push_change(FsChange::upsert(root.join("proj").join("app.log"))); // junk
        batch.push_change(FsChange::upsert(
            root.join("proj").join("node_modules").join("pkg").join("a.js"),
        )); // dentro de cleanup
        batch.push_change(FsChange::upsert(root.join("proj").join("node_modules"))); // marcador

        let stats = idx.apply_changes(&batch, &[], &rules()).unwrap();
        assert_eq!(stats.upserted, 1); // só o marcador node_modules
        assert_eq!(stats.skipped, 2); // *.log + filho de node_modules
        assert_eq!(idx.count(), 1);
    }

    #[test]
    fn apply_changes_removes_file_and_dir_tree() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let idx = IndexDb::open(&root.join("idx.db")).unwrap();

        // Semeia o índice com um projeto + subpasta + arquivos.
        let proj = root.join("proj");
        fs::create_dir_all(proj.join("sub")).unwrap();
        fs::write(proj.join("a.rs"), "x").unwrap();
        fs::write(proj.join("sub").join("b.rs"), "x").unwrap();
        let mut seed = ChangeBatch::new();
        seed.push_change(FsChange::upsert(proj.clone()));
        seed.push_change(FsChange::upsert(proj.join("a.rs")));
        seed.push_change(FsChange::upsert(proj.join("sub")));
        seed.push_change(FsChange::upsert(proj.join("sub").join("b.rs")));
        idx.apply_changes(&seed, &[], &rules()).unwrap();
        assert_eq!(idx.count(), 4);

        // Remove só um arquivo.
        let mut rm_file = ChangeBatch::new();
        rm_file.push_change(FsChange::remove(proj.join("a.rs")));
        idx.apply_changes(&rm_file, &[], &rules()).unwrap();
        assert_eq!(idx.count(), 3);

        // Remove a árvore inteira do projeto (dir + descendentes).
        let mut rm_tree = ChangeBatch::new();
        rm_tree.push_change(FsChange::remove(proj.clone()));
        let stats = idx.apply_changes(&rm_tree, &[], &rules()).unwrap();
        assert_eq!(stats.removed, 1);
        assert_eq!(idx.count(), 0);
    }

    #[test]
    fn apply_changes_coalesces_rename() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let idx = IndexDb::open(&root.join("idx.db")).unwrap();

        // Cria old.rs.
        fs::write(root.join("old.rs"), "x").unwrap();
        let mut seed = ChangeBatch::new();
        seed.push_change(FsChange::upsert(root.join("old.rs")));
        idx.apply_changes(&seed, &[], &rules()).unwrap();
        assert_eq!(idx.count(), 1);

        // Rename → remove(old) + create(new), no mesmo batch.
        fs::write(root.join("new.rs"), "x").unwrap();
        let mut rename = ChangeBatch::new();
        rename.push_change(FsChange::remove(root.join("old.rs")));
        rename.push_change(FsChange::upsert(root.join("new.rs")));
        let stats = idx.apply_changes(&rename, &[], &rules()).unwrap();
        assert_eq!(stats.upserted, 1);
        assert_eq!(stats.removed, 1);
        assert_eq!(idx.count(), 1);
    }

    #[test]
    fn apply_changes_detects_project_root() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // projeto com .git
        fs::create_dir_all(root.join("p").join(".git")).unwrap();
        fs::write(root.join("p").join(".git").join("HEAD"), "x").unwrap();
        fs::write(root.join("p").join("main.rs"), "x").unwrap();

        let idx = IndexDb::open(&root.join("idx.db")).unwrap();
        let mut batch = ChangeBatch::new();
        batch.push_change(FsChange::upsert(root.join("p").join("main.rs")));
        idx.apply_changes(&batch, &[], &rules()).unwrap();

        let entries = idx.list_all();
        let e = entries.iter().find(|e| e.path.ends_with("main.rs")).unwrap();
        assert!(e.project_root.as_deref().unwrap().ends_with("p"));
    }

    /// O scan paralelo deve indexar exatamente os mesmos paths que o walker sequencial
    /// (poda de node_modules, detecção de project root, skip de junk).
    #[test]
    fn parallel_scan_indexes_same_as_sequential_walker() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("proj").join(".git")).unwrap();
        fs::write(root.join("proj").join(".git").join("HEAD"), "x").unwrap();
        fs::write(root.join("proj").join("main.js"), "x").unwrap();
        fs::create_dir_all(root.join("proj").join("node_modules").join("pkg")).unwrap();
        fs::write(
            root.join("proj")
                .join("node_modules")
                .join("pkg")
                .join("a.js"),
            "x",
        )
        .unwrap();
        fs::write(root.join("proj").join("debug.log"), "x").unwrap(); // junk (*.log) → skip

        let targets = vec![
            CleanupTarget::dir("node_modules", "javascript"),
            CleanupTarget::files("log", &["*.log"], "logs"),
        ];
        let rules = CleanupRules::from_targets(&targets).unwrap();

        // Esperado: walker sequencial.
        let mut expected: Vec<String> = Vec::new();
        walker::for_each_entry(root, &[], &rules, |e| expected.push(e.path));
        expected.sort();

        // Real: scan paralelo (2 threads p/ exercitar o caminho paralelo).
        // DB num tempdir separado p/ não poluir a raiz escaneada com idx.db/-wal/-shm.
        let db_dir = tempdir().unwrap();
        let idx = IndexDb::open(&db_dir.path().join("idx.db")).unwrap();
        let stats = idx
            .scan(
                &[root.to_path_buf()],
                &[],
                &targets,
                2,
                None,
                |_| {},
            )
            .unwrap();
        assert!(stats.entries_indexed > 0);

        let mut actual: Vec<String> =
            idx.list_all().into_iter().map(|e| e.path).collect();
        actual.sort();
        assert_eq!(
            actual, expected,
            "scan paralelo deve indexar os mesmos paths do walker sequencial"
        );

        // project_root detectado no main.js; node_modules é marcador (podado o conteúdo).
        let entries = idx.list_all();
        let main = entries
            .iter()
            .find(|e| e.path.ends_with("main.js"))
            .expect("main.js indexado");
        assert!(main.project_root.as_deref().unwrap().ends_with("proj"));
        assert!(entries.iter().any(|e| e.path.ends_with("node_modules")));
        assert!(!entries.iter().any(|e| e.path.ends_with("a.js")));
        assert!(!entries.iter().any(|e| e.path.ends_with("debug.log"))); // junk skipado
    }

    /// Cancelamento: com o flag já setado, o scan para cedo e devolve stats parciais.
    #[test]
    fn parallel_scan_respects_cancel() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a").join("b")).unwrap();
        for i in 0..50 {
            fs::write(root.join("a").join("b").join(format!("f{i}.txt")), "x").unwrap();
        }
        // total esperado se rodasse até o fim: root + a + a/b + 50 = 53 entradas.
        let cancel = Arc::new(AtomicBool::new(true));
        let db_dir = tempdir().unwrap();
        let idx = IndexDb::open(&db_dir.path().join("idx.db")).unwrap();
        let stats = idx
            .scan(&[root.to_path_buf()], &[], &[], 2, Some(cancel), |_| {})
            .unwrap();
        assert!(
            stats.entries_indexed < 53,
            "cancel deve parar cedo; indexou {}",
            stats.entries_indexed
        );
    }
}
