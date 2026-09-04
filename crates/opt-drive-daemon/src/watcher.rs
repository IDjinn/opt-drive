//! File-watcher em tempo real: observa `watch.paths` e aplica mudanças incrementais
//! ao índice SQLite (sem re-scan completo).
//!
//! Usa `notify-debouncer-mini`, que coalesce bursts de eventos do filesystem
//! (`cargo build`/`npm install` tocam milhares de arquivos). O debouncer mini **não**
//! distingue create/modify/remove (apenas "algo mudou"); portanto marcamos todos os
//! eventos como `Upsert` e deixamos `Indexer::apply_changes` decidir via `stat`:
//! - path existe agora → (re)indexa;
//! - path sumiu → remove do índice (+ descendentes).
//!
//! Isso reflete o **estado final real** de cada path após a janela de debounce — mais
//! robusto que interpretar kinds de evento.
//!
//! Arquitetura:
//! - thread própria ("opt-drive-watcher") detém o `Debouncer` (precisa ficar vivo) e
//!   drena o channel de eventos;
//! - para cada lote debounced, entrega o trabalho ao runtime tokio via `Handle::spawn`,
//!   que roda o I/O de DB em `spawn_blocking` (SQLite é síncrono) e emite
//!   `Event::IndexUpdated`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult};

use opt_drive_core::cleanup_catalog::CleanupRules;
use opt_drive_core::index::{ChangeBatch, ChangeStats, FsChange, Indexer};

use crate::state::{ChangeStatsDto, Event, AppState};

/// Handle para parar o watcher (a thread encerra em até ~500ms).
#[derive(Clone)]
pub struct WatcherHandle {
    stop: Arc<AtomicBool>,
}

impl WatcherHandle {
    /// Sinaliza parada (idempotente). O watcher libera os watches ao encerrar.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Inicia o file-watcher (se habilitado na config). Retorna `None` quando
/// `watch.realtime = false` ou não há `watch.paths`; o watcher roda numa thread
/// própria e pode ser trocado em runtime via [`WatcherHandle::stop`] + novo
/// `start` (usado pelo `PUT /api/config`).
pub fn start(state: AppState) -> anyhow::Result<Option<WatcherHandle>> {
    let cfg = state.load_config()?;
    if !cfg.watch.realtime {
        tracing::info!(
            target: "opt-drive.watch",
            "watcher desligado pela config (watch.realtime = false)"
        );
        return Ok(None);
    }
    if cfg.watch.paths.is_empty() {
        tracing::info!(
            target: "opt-drive.watch",
            "watcher ocioso (nenhum watch.path configurado)"
        );
        return Ok(None);
    }

    // Clamp defensativo: janela útil entre 50ms e 5s.
    let debounce_ms = cfg.watch.debounce_ms.clamp(50, 5_000);
    let paths: Vec<PathBuf> = cfg.watch.paths.clone();
    let ignore_globs = cfg.watch.ignore_globs.clone();
    let rules = Arc::new(
        CleanupRules::from_targets(&cfg.cleanup.effective_targets())
            .unwrap_or_else(|_| CleanupRules::empty()),
    );
    let db_path = state.db_path.clone();
    let events = state.events.clone();
    let runtime = tokio::runtime::Handle::try_current()?;

    let handle = WatcherHandle {
        stop: Arc::new(AtomicBool::new(false)),
    };
    let stop = handle.stop.clone();

    std::thread::Builder::new()
        .name("opt-drive-watcher".into())
        .spawn(move || {
            watcher_loop(
                paths,
                debounce_ms,
                ignore_globs,
                rules,
                db_path,
                events,
                runtime,
                stop,
            )
        })?;

    Ok(Some(handle))
}

#[allow(clippy::too_many_arguments)]
fn watcher_loop(
    paths: Vec<PathBuf>,
    debounce_ms: u64,
    ignore_globs: Vec<String>,
    rules: Arc<CleanupRules>,
    db_path: PathBuf,
    events: tokio::sync::broadcast::Sender<Event>,
    runtime: tokio::runtime::Handle,
    stop: Arc<AtomicBool>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<DebounceEventResult>();

    let mut debouncer = match new_debouncer(Duration::from_millis(debounce_ms), move |res| {
        let _ = tx.send(res);
    }) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(
                target: "opt-drive.watch",
                error = %e,
                "não foi possível iniciar o watcher"
            );
            return;
        }
    };

    let mut watched = 0usize;
    for p in &paths {
        match debouncer.watcher().watch(p.as_path(), RecursiveMode::Recursive) {
            Ok(()) => watched += 1,
            Err(e) => tracing::warn!(
                target: "opt-drive.watch",
                path = ?p,
                error = %e,
                "falha ao observar path"
            ),
        }
    }
    if watched == 0 {
        tracing::error!(
            target: "opt-drive.watch",
            "nenhum path pôde ser observado — watcher inativo"
        );
        return;
    }
    tracing::info!(target: "opt-drive.watch", watched, debounce_ms, "watcher ativo");

    // O `debouncer` vive nesta thread; o loop abaixo o mantém vivo até receber a
    // ordem de parada (troca de config) ou o processo encerrar. O polling de 500ms
    // no `recv_timeout` é o que permite observar a flag `stop`.
    loop {
        if stop.load(Ordering::Relaxed) {
            tracing::info!(target: "opt-drive.watch", "watcher parado (config trocada)");
            break;
        }
        let res = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(r) => r,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let event_vec = match res {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "opt-drive.watch", error = %e, "erro do watcher ignorado");
                continue;
            }
        };

        let mut batch = ChangeBatch::new();
        for e in event_vec {
            // O debouncer mini não distingue create/modify/remove: marquemos tudo como
            // Upsert. `apply_changes` faz stat e decide: existe → upsert, sumiu → remove.
            batch.push_change(FsChange::upsert(e.path));
        }
        if batch.is_empty() {
            continue;
        }

        let n = batch.len();
        let ignore = ignore_globs.clone();
        let rules = rules.clone();
        let db_path = db_path.clone();
        let events = events.clone();

        // Entrega o lote ao runtime tokio: o I/O de DB roda em `spawn_blocking`.
        runtime.spawn(async move {
            let res = tokio::task::spawn_blocking(move || -> anyhow::Result<ChangeStats> {
                let indexer = Indexer::open(&db_path)?;
                let stats = indexer.apply_changes(&batch, &ignore, &rules)?;
                let _ = indexer.db.touch_indexed();
                Ok(stats)
            })
            .await;
            match res {
                Ok(Ok(stats)) => {
                    if stats.upserted + stats.removed > 0 {
                        tracing::debug!(
                            target: "opt-drive.watch",
                            events = n,
                            upserted = stats.upserted,
                            removed = stats.removed,
                            skipped = stats.skipped,
                            "incremento aplicado"
                        );
                        let _ = events.send(Event::IndexUpdated {
                            stats: ChangeStatsDto::from(stats),
                        });
                    }
                }
                Ok(Err(e)) => tracing::warn!(
                    target: "opt-drive.watch",
                    error = %e,
                    "falha ao aplicar incremento"
                ),
                Err(e) => tracing::warn!(
                    target: "opt-drive.watch",
                    error = %e,
                    "join do incremento falhou"
                ),
            }
        });
    }

    tracing::info!(target: "opt-drive.watch", "watcher encerrado");
}
