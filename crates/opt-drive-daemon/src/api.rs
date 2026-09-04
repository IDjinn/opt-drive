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
        .route("/api/backup/run", post(backup_run))
        .route("/api/backup/status", get(backup_status))
        .route("/api/backup/restore", post(backup_restore))
        .route("/api/events", get(events_ws))
        .layer(cors)
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn drives_handler(State(st): State<AppState>) -> R<Vec<Drive>> {
    let cfg = st.load_config()?;
    Ok(Json(st.cached_drives(&cfg).await?))
}

async fn get_config(State(st): State<AppState>) -> R<Config> {
    Ok(Json(st.load_config()?))
}

async fn put_config(State(st): State<AppState>, Json(cfg): Json<Config>) -> R<serde_json::Value> {
    cfg.save(&st.config_path)?;
    // Reinicia o file-watcher com os paths novos — ele só lia a config no startup;
    // sem isto, adicionar/remover drives da indexação só valeria após reiniciar.
    if let Some(h) = st.watcher.lock().unwrap().take() {
        h.stop();
    }
    match crate::watcher::start(st.clone()) {
        Ok(h) => *st.watcher.lock().unwrap() = h,
        Err(e) => tracing::warn!(target: "opt-drive.watch", error = %e, "watcher não reiniciou"),
    }
    Ok(Json(json!({ "saved": true })))
}

/// Dispara a varredura completa como job em background: valida a config, garante
/// exclusão via `run_lock` (409 se algo já roda) e responde `{"started":true}` na
/// hora. Progresso e término chegam via WS (`ScanProgress`/`ScanDone`/…).
async fn index_run(State(st): State<AppState>) -> R<serde_json::Value> {
    let cfg = st.load_config()?;
    if cfg.watch.paths.is_empty() {
        return Err(AppError::msg(StatusCode::BAD_REQUEST, "nenhum watch.path configurado"));
    }

    // Não espera na fila: com scan/tier/backup em andamento, falha rápido com 409
    // (antes o request ficava pendurado até o job anterior terminar).
    let lock = st.run_lock.clone().try_lock_owned().map_err(|_| {
        AppError::msg(StatusCode::CONFLICT, "uma operação pesada já está em andamento (scan/tier/backup)")
    })?;

    // Cria a flag de cancelamento e registra no slot (sempre limpa ao sair).
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut slot = st.scan_cancel.lock().unwrap();
        *slot = Some(cancel.clone());
    }

    let st_for_task = st.clone();
    tokio::spawn(async move {
        run_scan(st_for_task, &cfg, lock, cancel).await;
    });

    Ok(Json(json!({ "started": true })))
}

/// Corpo do scan (task em background; o HTTP já respondeu). Garante evento
/// terminal em **todos** os caminhos — sem isso a UI fica presa em "Indexando…".
async fn run_scan(
    st: AppState,
    cfg: &Config,
    _lock: tokio::sync::OwnedMutexGuard<()>,
    cancel: Arc<AtomicBool>,
) {
    // Estimativa de total (do último scan) — permite à UI desenhar a barra imediatamente.
    // (Leitura pura; falha de join só deixa sem estimativa.)
    let db_for_est = st.index_db.clone();
    let total_estimate = spawn_blocking(move || -> anyhow::Result<Option<usize>> {
        Ok(db_for_est.scan_total_estimate())
    })
    .await
    .unwrap_or(None);
    st.emit(Event::ScanStarted { total_estimate });

    let threads = cfg.indexer.threads;
    let events = st.events.clone();
    let paths = cfg.watch.paths.clone();
    let globs = cfg.watch.ignore_globs.clone();
    let cleanup = cfg.cleanup.effective_targets();
    let db = st.index_db.clone();
    let cancel_for_task = cancel.clone();

    let join = tokio::task::spawn_blocking(
        move || -> anyhow::Result<opt_drive_core::index::ScanStats> {
            db.scan(&paths, &globs, &cleanup, threads, Some(cancel_for_task), |p| {
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

    let end = match join {
        Ok(Ok(stats)) => ScanEnd::Done(stats),
        Ok(Err(e)) => ScanEnd::Failed(format!("{e:#}")),
        Err(e) => ScanEnd::Failed(format!("join: {e}")),
    };
    finish_scan(&st, &cancel, end);
}

/// Desfecho possível de uma varredura (cancelamento é detectado pela flag no
/// `finish_scan` — o scan cancelado ainda retorna `Ok(stats)` parciais).
enum ScanEnd {
    Done(opt_drive_core::index::ScanStats),
    Failed(String),
}

/// Limpa o slot de cancelamento e emite o evento terminal (sempre chamado).
fn finish_scan(st: &AppState, cancel: &AtomicBool, end: ScanEnd) {
    {
        let mut slot = st.scan_cancel.lock().unwrap();
        *slot = None;
    }
    let canceled = cancel.load(Ordering::Relaxed);
    match end {
        ScanEnd::Done(stats) => {
            if canceled {
                st.emit(Event::ScanCanceled);
            } else {
                st.emit(Event::ScanDone {
                    stats: ScanStatsDto::from(stats),
                });
            }
        }
        ScanEnd::Failed(e) => {
            tracing::error!(target: "opt-drive.index", error = %e, "varredura falhou");
            st.emit(Event::ScanFailed { error: e });
        }
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
    // `..` só importa como componente (pastas como `my..folder` são válidas).
    let path = PathBuf::from(&q.path);
    if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(AppError::msg(StatusCode::BAD_REQUEST, "caminho inválido (..)"));
    }
    if !path.is_absolute() {
        return Err(AppError::msg(StatusCode::BAD_REQUEST, "caminho deve ser absoluto"));
    }
    // Canonicaliza (resolve `.`/junctions), remove o prefixo verbatim `\\?\` (as
    // chaves do índice são plain) e confere que está sob um drive conhecido.
    let canon = opt_drive_core::browse::normalize_verbatim(&path.canonicalize().unwrap_or(path));
    let drives = st.cached_drives(&cfg).await?;
    let mount = drives::mount_of(&canon);
    if !matches!(mount.as_deref(), Some(m) if drives.iter().any(|d| d.mount == m)) {
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
    let drives = st.cached_drives(&cfg).await?;

    let st2 = st.clone();
    let plan = spawn_blocking(move || -> anyhow::Result<Plan> {
        let db = IndexDb::open(&st2.db_path)?;
        let entries = db.list_all();
        let scorer = ActivityScorer::new();
        Ok(policy::plan(&cfg.rules, &entries, &drives, &scorer, unix_now(), &cfg.protected_paths))
    })
    .await?;

    st.emit(Event::TierPreview { plan: plan.clone() });
    Ok(Json(plan))
}

async fn tier_apply(State(st): State<AppState>) -> R<RunReport> {
    let cfg = st.load_config()?;
    let drives = st.cached_drives(&cfg).await?;

    // 409 se houver scan/backup em andamento — antes o request pendurava no
    // `run_lock` até o outro job terminar (UI em "Aplicando…" indefinidamente).
    let _lock = st.run_lock.clone().try_lock_owned().map_err(|_| {
        AppError::msg(StatusCode::CONFLICT, "uma operação pesada já está em andamento (scan/tier/backup)")
    })?;
    st.emit(Event::TierStarted);

    let st2 = st.clone();
    let journal_path = journal_dir().join(format!("{}.json", id()));
    let journal_for_task = journal_path.clone();
    let report = spawn_blocking(move || -> anyhow::Result<RunReport> {
        let db = IndexDb::open(&st2.db_path)?;
        let entries = db.list_all();
        let scorer = ActivityScorer::new();
        let plan =
            policy::plan(&cfg.rules, &entries, &drives, &scorer, unix_now(), &cfg.protected_paths);

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

/// Executa o backup configurado em `[backup]` (sync incremental de todos os
/// caminhos). Segue o padrão do tier_apply: `run_lock` + `spawn_blocking` +
/// eventos WS.
async fn backup_run(State(st): State<AppState>) -> R<opt_drive_core::providers::SyncReport> {
    let cfg = st.load_config()?;
    if !cfg.backup.enabled() {
        return Err(AppError::msg(
            StatusCode::BAD_REQUEST,
            "backup não configurado: defina [backup] connector e paths",
        ));
    }
    // Passphrase antes de travar (nunca logar o valor).
    let passphrase = if cfg.backup.encrypt {
        Some(cfg.backup.passphrase()?)
    } else {
        None
    };

    // 409 se houver scan/tier em andamento (mesma política dos outros jobs pesados).
    let _lock = st.run_lock.clone().try_lock_owned().map_err(|_| {
        AppError::msg(StatusCode::CONFLICT, "uma operação pesada já está em andamento (scan/tier/backup)")
    })?;
    st.emit(Event::BackupStarted {
        connector: cfg.backup.connector.clone(),
        paths: cfg.backup.paths.len(),
    });

    let st2 = st.clone();
    let backup = cfg.backup.clone();
    let report = spawn_blocking(move || -> anyhow::Result<opt_drive_core::providers::SyncReport> {
        let provider = opt_drive_connectors::connector_from_config(&backup)?;
        let db = st2.index_db.clone();
        let events = st2.events.clone();

        let mut total = opt_drive_core::providers::SyncReport::default();
        for path in &backup.paths {
            let ctx = opt_drive_core::providers::sync::SyncContext {
                db: &db,
                delete_remote: backup.delete_remote,
                passphrase: passphrase.as_deref(),
                progress: &|current, frac| {
                    let _ = events.send(Event::BackupProgress {
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
    })
    .await?;

    st.emit(Event::BackupDone {
        report: report.clone(),
        errors: report.failed as usize,
    });
    Ok(Json(report))
}

/// Estado do backup: config resumida + nº de itens no manifest.
async fn backup_status(State(st): State<AppState>) -> R<serde_json::Value> {
    let cfg = st.load_config()?;
    let db_path = st.db_path.clone();
    let entries = spawn_blocking(move || -> anyhow::Result<i64> {
        let db = IndexDb::open(&db_path)?;
        let n = db.backup_state_count();
        Ok(n)
    })
    .await?;
    Ok(Json(json!({
        "enabled": cfg.backup.enabled(),
        "connector": cfg.backup.connector,
        "paths": cfg.backup.paths,
        "encrypt": cfg.backup.encrypt,
        "delete_remote": cfg.backup.delete_remote,
        "schedule_secs": cfg.backup.schedule_secs,
        "entries": entries,
    })))
}

#[derive(serde::Deserialize)]
struct RestoreBody {
    /// Identificador no destino (coluna `remote_id` do `backup_state`).
    remote_id: String,
    /// Caminho local de destino do arquivo restaurado.
    dst: String,
}

async fn backup_restore(
    State(st): State<AppState>,
    Json(body): Json<RestoreBody>,
) -> R<serde_json::Value> {
    let cfg = st.load_config()?;
    if !cfg.backup.enabled() {
        return Err(AppError::msg(
            StatusCode::BAD_REQUEST,
            "backup não configurado: defina [backup] connector e paths",
        ));
    }
    let passphrase = if cfg.backup.encrypt {
        Some(cfg.backup.passphrase()?)
    } else {
        None
    };

    let backup = cfg.backup.clone();
    let dst = PathBuf::from(&body.dst);
    let remote_id = body.remote_id.clone();
    let db = st.index_db.clone();
    spawn_blocking(move || -> anyhow::Result<()> {
        let provider = opt_drive_connectors::connector_from_config(&backup)?;
        let ctx = opt_drive_core::providers::sync::SyncContext {
            db: &db,
            delete_remote: false,
            passphrase: passphrase.as_deref(),
            progress: &|_, _| {},
        };
        provider.restore(&remote_id, &dst, &ctx)
    })
    .await?;
    Ok(Json(json!({ "restored": true, "dst": body.dst })))
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
