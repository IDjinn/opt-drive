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

    // File-watcher em tempo real (indexação incremental). Best-effort: se falhar, o
    // daemon continua com o scan completo periódico do scheduler.
    if let Err(e) = watcher::start(state.clone()) {
        tracing::warn!(target: "opt-drive.watch", error = %e, "watcher não iniciado");
    }

    // Scheduler: re-index periódico (não aplica tiering automaticamente).
    if args.scan_interval > 0 {
        let sched_state = state.clone();
        let interval = Duration::from_secs(args.scan_interval);
        tokio::spawn(async move {
            scheduler(sched_state, interval).await;
        });
    }

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
            let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
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
                let _ = events.send(state::Event::ScanDone {
                    stats: state::ScanStatsDto::from(stats),
                });
                Ok(())
            })
            .await;
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
