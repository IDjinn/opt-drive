//! Handlers da API REST + WebSocket do daemon.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;

use opt_drive_core::browse::DirEntry;
use opt_drive_core::cleanup_catalog::CleanupTarget;
use opt_drive_core::config::Config;
use opt_drive_core::drives::{self, Drive};
use opt_drive_core::index::IndexDb;
use opt_drive_core::ops::{Executor, RunReport};
use opt_drive_core::policy::{self, Plan};
use opt_drive_core::usage::{unix_now, ActivityScorer};

use crate::state::{Event, ScanStatsDto, AppState};

pub fn router(state: AppState) -> Router {
    // CORS permissivo: o daemon é só localhost; o Electron (dev em 5173, prod em file://)
    // precisa chamar a API de outra origem.
    let cors = tower_http::cors::CorsLayer::permissive();
    Router::new()
        .route("/api/health", get(health))
        .route("/api/drives", get(drives_handler))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/index/run", post(index_run))
        .route("/api/index/cancel", post(index_cancel))
        .route("/api/index/status", get(index_status))
        .route("/api/browse", get(browse))
        .route("/api/cleanup/catalog", get(cleanup_catalog))
        .route("/api/tier/preview", get(tier_preview))
        .route("/api/tier/apply", post(tier_apply))
        .route("/api/journals", get(list_journals))
        .route("/api/events", get(events_ws))
        .layer(cors)
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn drives_handler(State(st): State<AppState>) -> R<Vec<Drive>> {
    let cfg = st.load_config()?;
    Ok(Json(drives::enumerate(|m| cfg.tier_for(m))))
}

async fn get_config(State(st): State<AppState>) -> R<Config> {
    Ok(Json(st.load_config()?))
}

async fn put_config(State(st): State<AppState>, Json(cfg): Json<Config>) -> R<serde_json::Value> {
    cfg.save(&st.config_path)?;
    Ok(Json(json!({ "saved": true })))
}

async fn index_run(State(st): State<AppState>) -> R<serde_json::Value> {
    let cfg = st.load_config()?;
    if cfg.watch.paths.is_empty() {
        return Err(AppError::msg(StatusCode::BAD_REQUEST, "nenhum watch.path configurado"));
    }

    let _lock = st.run_lock.clone().lock_owned().await;

    // Cria a flag de cancelamento e registra no slot (sempre limpa ao sair).
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut slot = st.scan_cancel.lock().unwrap();
        *slot = Some(cancel.clone());
    }

    // Estimativa de total (do último scan) — permite à UI desenhar a barra imediatamente.
    let db_path_for_est = st.db_path.clone();
    let total_estimate = spawn_blocking(move || -> anyhow::Result<Option<usize>> {
        Ok(IndexDb::open(&db_path_for_est)?.scan_total_estimate())
    })
    .await?;
    st.emit(Event::ScanStarted { total_estimate });

    let threads = cfg.indexer.threads;
    let events = st.events.clone();
    let paths = cfg.watch.paths.clone();
    let globs = cfg.watch.ignore_globs.clone();
    let cleanup = cfg.cleanup.effective_targets();
    let db_path = st.db_path.clone();
    let cancel_for_task = cancel.clone();

    // spawn_blocking direto (sem o helper) p/ podermos limpar o slot e emitir
    // ScanCanceled/ScanDone em todos os caminhos (incl. erro).
    let join = tokio::task::spawn_blocking(
        move || -> anyhow::Result<opt_drive_core::index::ScanStats> {
            let indexer = opt_drive_core::index::Indexer::open(&db_path)?;
            indexer.scan(&paths, &globs, &cleanup, threads, Some(cancel_for_task), |p| {
                let _ = events.send(Event::ScanProgress {
                    indexed: p.indexed,
                    current_dir: p.current_dir.clone(),
                    total_estimate: p.total_estimate,
                    elapsed_ms: p.elapsed_ms,
                    bytes: p.bytes,
                    errors: p.errors,
                });
                // Log periódico no terminal do daemon.
                if p.indexed > 0 && p.indexed % 20000 == 0 {
                    tracing::info!(
                        target: "opt-drive.index",
                        indexed = p.indexed,
                        "indexando…"
                    );
                }
            })
        },
    )
    .await;

    // Limpa o slot sempre (mesmo em erro/cancel).
    {
        let mut slot = st.scan_cancel.lock().unwrap();
        *slot = None;
    }

    let canceled = cancel.load(Ordering::Relaxed);
    match join {
        Ok(Ok(stats)) => {
            if canceled {
                st.emit(Event::ScanCanceled);
            } else {
                st.emit(Event::ScanDone {
                    stats: ScanStatsDto::from(stats),
                });
            }
            Ok(Json(json!({ "started": true, "canceled": canceled })))
        }
        Ok(Err(e)) => Err(AppError::from(e)),
        Err(e) => Err(AppError::msg(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("join: {e}"),
        )),
    }
}

/// Cancela a varredura em andamento (se houver). Retorna `{canceled: bool}` — `false`
/// quando não há scan rodando.
async fn index_cancel(State(st): State<AppState>) -> R<serde_json::Value> {
    let canceled = {
        let slot = st.scan_cancel.lock().unwrap();
        if let Some(flag) = slot.as_ref() {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    };
    Ok(Json(json!({ "canceled": canceled })))
}

async fn index_status(State(st): State<AppState>) -> R<serde_json::Value> {
    let db_path = st.db_path.clone();
    let (count, last) = spawn_blocking(move || -> anyhow::Result<(usize, Option<i64>)> {
        let db = IndexDb::open(&db_path)?;
        Ok((db.count(), db.last_indexed()))
    })
    .await?;

    Ok(Json(json!({ "entries": count, "last_indexed": last })))
}

/// Lista os filhos de um diretório (estrutura ao vivo + tamanhos do índice).
///
/// Validação: o caminho deve ser absoluto, sem `..`, e estar sob um drive enumerado
/// (fixo/removível). Não toma o `run_lock` — é read-only e pode rodar em paralelo com
/// a indexação.
async fn browse(
    State(st): State<AppState>,
    Query(q): Query<BrowseQuery>,
) -> R<Vec<DirEntry>> {
    let cfg = st.load_config()?;
    if q.path.is_empty() {
        return Err(AppError::msg(StatusCode::BAD_REQUEST, "path vazio"));
    }
    if q.path.contains("..") {
        return Err(AppError::msg(StatusCode::BAD_REQUEST, "caminho inválido (..)"));
    }
    let path = PathBuf::from(&q.path);
    if !path.is_absolute() {
        return Err(AppError::msg(StatusCode::BAD_REQUEST, "caminho deve ser absoluto"));
    }
    // Canonicaliza (resolve `.`/junctions) e confere que está sob um drive conhecido.
    let canon = path.canonicalize().unwrap_or(path);
    let mounts: Vec<String> = drives::enumerate(|m| cfg.tier_for(m))
        .into_iter()
        .map(|d| d.mount)
        .collect();
    let mount = drives::mount_of(&canon);
    if !matches!(mount.as_deref(), Some(m) if mounts.iter().any(|x| x == m)) {
        return Err(AppError::msg(
            StatusCode::BAD_REQUEST,
            "caminho fora dos drives monitorados",
        ));
    }

    let db_path = st.db_path.clone();
    let entries = spawn_blocking(move || -> anyhow::Result<Vec<DirEntry>> {
        let db = IndexDb::open(&db_path)?;
        opt_drive_core::browse::list_dir(&canon, Some(&db))
    })
    .await?;

    Ok(Json(entries))
}

#[derive(serde::Deserialize)]
struct BrowseQuery {
    path: String,
}

/// Catálogo de cleanup: builtin (sempre completo, para a UI mostrar os toggles) +
/// alvos custom do usuário. O estado de seleção (ligado/desligado) vem do
/// `config.cleanup.disabled` — a UI cruza os dois.
async fn cleanup_catalog(State(st): State<AppState>) -> R<Vec<CleanupTarget>> {
    let cfg = st.load_config()?;
    let mut out = opt_drive_core::cleanup_catalog::default_catalog();
    for entry in &cfg.cleanup.targets {
        out.push(entry.clone().into_target());
    }
    Ok(Json(out))
}

async fn tier_preview(State(st): State<AppState>) -> R<Plan> {
    let cfg = st.load_config()?;
    let drives = drives::enumerate(|m| cfg.tier_for(m));

    let st2 = st.clone();
    let plan = spawn_blocking(move || -> anyhow::Result<Plan> {
        let db = IndexDb::open(&st2.db_path)?;
        let entries = db.list_all();
        let scorer = ActivityScorer::new();
        Ok(policy::plan(&cfg.rules, &entries, &drives, &scorer, unix_now()))
    })
    .await?;

    st.emit(Event::TierPreview { plan: plan.clone() });
    Ok(Json(plan))
}

async fn tier_apply(State(st): State<AppState>) -> R<RunReport> {
    let cfg = st.load_config()?;
    let drives = drives::enumerate(|m| cfg.tier_for(m));

    let _lock = st.run_lock.clone().lock_owned().await;
    st.emit(Event::TierStarted);

    let st2 = st.clone();
    let journal_path = journal_dir().join(format!("{}.json", id()));
    let journal_for_task = journal_path.clone();
    let report = spawn_blocking(move || -> anyhow::Result<RunReport> {
        let db = IndexDb::open(&st2.db_path)?;
        let entries = db.list_all();
        let scorer = ActivityScorer::new();
        let plan = policy::plan(&cfg.rules, &entries, &drives, &scorer, unix_now());

        let exec = Executor::new(cfg.cleanup.effective_targets(), false);
        let events = st2.events.clone();
        let report = exec.execute(&plan, &journal_for_task, |desc, frac| {
            let _ = events.send(Event::TierProgress {
                desc: desc.to_string(),
                frac,
            });
        })?;
        Ok(report)
    })
    .await?;

    st.emit(Event::TierDone { report: report.clone() });
    Ok(Json(report))
}

async fn list_journals(State(_st): State<AppState>) -> R<serde_json::Value> {
    let dir = journal_dir();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    Ok(Json(json!({ "dir": dir, "journals": names })))
}

/// WebSocket de eventos ao vivo.
async fn events_ws(
    ws: axum::extract::ws::WebSocketUpgrade,
    State(st): State<AppState>,
) -> Response {
    ws.on_upgrade(move |mut socket| async move {
        use axum::extract::ws::Message;

        let mut rx = st.events.subscribe();
        let _ = socket
            .send(Message::Text(
                serde_json::to_string(&Event::Hello { version: "0.1.0" }).unwrap_or_default(),
            ))
            .await;

        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(event) => {
                        let txt = serde_json::to_string(&event).unwrap_or_default();
                        if socket.send(Message::Text(txt)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                },
            }
        }
    })
}

// --- erros / helpers ----------------------------------------------------------

/// Erro da API convertido em resposta HTTP.
pub struct AppError {
    status: StatusCode,
    msg: String,
}

impl AppError {
    fn msg(status: StatusCode, msg: impl Into<String>) -> Self {
        Self {
            status,
            msg: msg.into(),
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::msg(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, self.msg).into_response()
    }
}

type R<T> = Result<Json<T>, AppError>;

/// Roda `f` em `spawn_blocking` e achata os dois níveis de Result.
async fn spawn_blocking<F, T>(f: F) -> Result<T, AppError>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(AppError::from(e)),
        Err(e) => Err(AppError::msg(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("join: {e}"),
        )),
    }
}

fn journal_dir() -> PathBuf {
    let proj = directories::ProjectDirs::from("dev", "optdrive", "opt-drive")
        .expect("dir de dados do SO");
    let dir = proj.data_dir().join("journals");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", n)
}
