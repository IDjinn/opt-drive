//! opt-drive daemon — serviço background (REST + WebSocket) com scheduler.
//!
//! Liga-se a `127.0.0.1` numa porta efêmera (ou fixa via `--port`) e imprime em
//! stdout um marcador parseável para o Electron descobrir a porta:
//!
//! ```text
//! OPTDRIVE_LISTENING {"port":54321,"host":"127.0.0.1"}
//! ```

mod api;
mod state;
mod watcher;

use std::time::Duration;

use clap::Parser;
use opt_drive_core::config::Config;
use state::AppState;

#[derive(Parser)]
#[command(name = "opt-drive-daemon", version, about = "Daemon opt-drive")]
struct Args {
    /// Porta (0 = efêmera, escolhida pelo SO).
    #[arg(long, default_value_t = 0)]
    port: u16,

    /// Intervalo do re-index automático, em segundos (0 = desabilita).
    #[arg(long, default_value_t = 3600)]
    scan_interval: u64,

    /// Caminho do config.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Caminho do banco de índice.
    #[arg(long)]
    db: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // `opt_drive` = targets derivados do nome do crate (underscores);
                // `opt-drive` = alvos explícitos pela convenção do AGENTS.md (hífens).
                .unwrap_or_else(|_| "opt_drive=info,opt-drive=info,warn".into()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(opt_drive_core::default_config_path);
    let db_path = args
        .db
        .clone()
        .unwrap_or_else(opt_drive_core::default_db_path);

    // Garante config e DB iniciais.
    let _ = Config::load_or_create(&config_path)?;

    let state = AppState::new(config_path.clone(), db_path.clone());

    // Pré-aquece o cache de drives (PowerShell demora segundos; sem isto o 1º
    // browse da UI paga o custo).
    {
        let warm = state.clone();
        tokio::spawn(async move {
            if let Ok(cfg) = warm.load_config() {
                let _ = warm.cached_drives(&cfg).await;
            }
        });
    }

    // File-watcher em tempo real (indexação incremental). Best-effort: se falhar, o
    // daemon continua com o scan completo periódico do scheduler. O handle fica no
    // state p/ permitir a troca quando a config muda (PUT /api/config).
    match watcher::start(state.clone()) {
        Ok(h) => *state.watcher.lock().unwrap() = h,
        Err(e) => tracing::warn!(target: "opt-drive.watch", error = %e, "watcher não iniciou"),
    }

    // Scheduler: re-index periódico (não aplica tiering automaticamente).
    if args.scan_interval > 0 {
        let sched_state = state.clone();
        let interval = Duration::from_secs(args.scan_interval);
        tokio::spawn(async move {
            scheduler(sched_state, interval).await;
        });
    }

    // Scheduler de backup: roda quando [backup].schedule_secs > 0.
    tokio::spawn(backup_scheduler(state.clone()));

    // Bind efêmero.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", args.port)).await?;
    let local = listener.local_addr()?;
    tracing::info!("daemon ouvindo em http://{}", local);

    // Marcador parseável para o Electron.
    println!(
        "OPTDRIVE_LISTENING {{\"port\":{},\"host\":\"127.0.0.1\"}}",
        local.port()
    );
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let app = api::router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Loop do scheduler: a cada `interval`, se ocioso, re-indexa.
async fn scheduler(state: AppState, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await; // primeiro tick imediato
    loop {
        ticker.tick().await;
        // Só roda se não houver outra tarefa pesada em andamento.
        if let Ok(_guard) = state.run_lock.clone().try_lock_owned() {
            let cfg = match state.load_config() {
                Ok(c) => c,
                Err(_) => continue,
            };
            if cfg.watch.paths.is_empty() {
                continue;
            }
            let events = state.events.clone();
            let paths = cfg.watch.paths.clone();
            let globs = cfg.watch.ignore_globs.clone();
            let cleanup = cfg.cleanup.effective_targets();
            let threads = cfg.indexer.threads;
            let db_path = state.db_path.clone();
            state.emit(state::Event::ScanStarted { total_estimate: None });
            let res = tokio::task::spawn_blocking(move || -> anyhow::Result<state::ScanStatsDto> {
                let indexer = opt_drive_core::index::Indexer::open(&db_path)?;
                let stats = indexer.scan(&paths, &globs, &cleanup, threads, None, |p| {
                    let _ = events.send(state::Event::ScanProgress {
                        indexed: p.indexed,
                        current_dir: p.current_dir.clone(),
                        total_estimate: p.total_estimate,
                        elapsed_ms: p.elapsed_ms,
                        bytes: p.bytes,
                        errors: p.errors,
                    });
                })?;
                Ok(state::ScanStatsDto::from(stats))
            })
            .await;
            // Evento terminal sempre (sem isso a UI fica presa em "Indexando…").
            match res {
                Ok(Ok(stats)) => {
                    state.emit(state::Event::ScanDone { stats });
                }
                Ok(Err(e)) => {
                    tracing::error!(target: "opt-drive.index", error = %e, "varredura agendada falhou");
                    state.emit(state::Event::ScanFailed { error: format!("{e:#}") });
                }
                Err(e) => {
                    tracing::error!(target: "opt-drive.index", error = %e, "join da varredura falhou");
                    state.emit(state::Event::ScanFailed { error: format!("join: {e}") });
                }
            }
        }
    }
}

/// Loop do scheduler de backup: checa a cada minuto se `schedule_secs` venceu
/// e, se o daemon estiver ocioso, executa o sync incremental.
async fn backup_scheduler(state: AppState) {
    let mut last_run = std::time::Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    ticker.tick().await; // primeiro tick imediato
    loop {
        ticker.tick().await;
        let cfg = match state.load_config() {
            Ok(c) => c,
            Err(_) => continue,
        };
        let every = Duration::from_secs(cfg.backup.schedule_secs);
        if !cfg.backup.enabled() || every.is_zero() || last_run.elapsed() < every {
            continue;
        }
        // Só roda se não houver outra tarefa pesada em andamento.
        if let Ok(_guard) = state.run_lock.clone().try_lock_owned() {
            last_run = std::time::Instant::now();
            // Passphrase pode não estar definida (daemon headless) — falha e loga.
            let passphrase = if cfg.backup.encrypt {
                match cfg.backup.passphrase() {
                    Ok(p) => Some(p),
                    Err(e) => {
                        tracing::warn!(target: "opt-drive.backup", error = %e, "backup agendado ignorado");
                        continue;
                    }
                }
            } else {
                None
            };
            state.emit(state::Event::BackupStarted {
                connector: cfg.backup.connector.clone(),
                paths: cfg.backup.paths.len(),
            });
            let events = state.events.clone();
            let backup = cfg.backup.clone();
            let db_path = state.db_path.clone();
            let res = tokio::task::spawn_blocking(
                move || -> anyhow::Result<opt_drive_core::providers::SyncReport> {
                    let provider = opt_drive_connectors::connector_from_config(&backup)?;
                    let db = opt_drive_core::index::IndexDb::open(&db_path)?;
                    let mut total = opt_drive_core::providers::SyncReport::default();
                    for path in &backup.paths {
                        let ctx = opt_drive_core::providers::sync::SyncContext {
                            db: &db,
                            delete_remote: backup.delete_remote,
                            passphrase: passphrase.as_deref(),
                            progress: &|current, frac| {
                                let _ = events.send(state::Event::BackupProgress {
                                    current: current.to_string(),
                                    frac,
                                });
                            },
                        };
                        let r = provider.sync_dir(path, &ctx)?;
                        total.uploaded += r.uploaded;
                        total.skipped += r.skipped;
                        total.failed += r.failed;
                        total.bytes_transferred += r.bytes_transferred;
                    }
                    Ok(total)
                },
            )
            .await;
            match res {
                Ok(Ok(report)) => {
                    tracing::info!(target: "opt-drive.backup",
                        uploaded = report.uploaded, skipped = report.skipped,
                        failed = report.failed, "backup agendado concluído");
                    state.emit(state::Event::BackupDone {
                        report,
                        errors: 0,
                    });
                }
                Ok(Err(e)) => {
                    tracing::error!(target: "opt-drive.backup", error = %e, "backup agendado falhou")
                }
                Err(e) => {
                    tracing::error!(target: "opt-drive.backup", error = %e, "join do backup falhou")
                }
            }
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("falha ao instalar handler de Ctrl+C");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("falha ao instalar handler SIGTERM")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Ctrl+C recebido, encerrando..."),
        _ = terminate => tracing::info!("SIGTERM recebido, encerrando..."),
    }
}
