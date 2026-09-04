//! Estado compartilhado do daemon + eventos do WebSocket.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{broadcast, Mutex as AsyncMutex};

use opt_drive_core::config::Config;
use opt_drive_core::drives::Drive;
use opt_drive_core::index::IndexDb;
use opt_drive_core::ops::RunReport;
use opt_drive_core::policy::Plan;

/// Por quanto tempo o cache de enumeração de drives é considerado fresco.
const DRIVES_TTL: Duration = Duration::from_secs(30);

/// Eventos transmitidos via WebSocket `/api/events`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Indica que o daemon está ativo (enviado ao conectar).
    Hello { version: &'static str },
    /// Varredura completa iniciada. `total_estimate` (do último scan) permite à UI
    /// desenhar a barra de progresso imediatamente.
    ScanStarted { total_estimate: Option<usize> },
    /// Progresso parcial da varredura. A UI deriva %, velocidade e ETA destes campos.
    ScanProgress {
        indexed: usize,
        current_dir: Option<String>,
        total_estimate: Option<usize>,
        elapsed_ms: u64,
        bytes: u64,
        errors: usize,
    },
    ScanDone { stats: ScanStatsDto },
    /// Varredura cancelada a pedido do usuário (parou cedo, com stats parciais).
    ScanCanceled,
    /// Varredura falhou com erro (evento terminal — a UI sai do estado "indexando").
    ScanFailed { error: String },
    /// Incremento de indexação em tempo real (file-watcher aplicou um lote).
    IndexUpdated { stats: ChangeStatsDto },
    TierPreview { plan: Plan },
    TierStarted,
    TierProgress { desc: String, frac: f64 },
    TierDone { report: RunReport },
    /// Backup iniciado (conector nome + nº de caminhos configurados).
    BackupStarted { connector: String, paths: usize },
    /// Progresso do backup (arquivo atual + fração 0..1).
    BackupProgress { current: String, frac: f64 },
    /// Backup concluído (relatório consolidado de todos os caminhos).
    BackupDone { report: opt_drive_core::providers::SyncReport, errors: usize },
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanStatsDto {
    pub roots_scanned: usize,
    pub entries_indexed: usize,
    pub dirs: usize,
    pub files: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangeStatsDto {
    pub upserted: usize,
    pub removed: usize,
    pub skipped: usize,
}

impl From<opt_drive_core::index::ChangeStats> for ChangeStatsDto {
    fn from(s: opt_drive_core::index::ChangeStats) -> Self {
        Self {
            upserted: s.upserted,
            removed: s.removed,
            skipped: s.skipped,
        }
    }
}

impl From<opt_drive_core::index::ScanStats> for ScanStatsDto {
    fn from(s: opt_drive_core::index::ScanStats) -> Self {
        Self {
            roots_scanned: s.roots_scanned,
            entries_indexed: s.entries_indexed,
            dirs: s.dirs,
            files: s.files,
            total_bytes: s.total_bytes,
        }
    }
}

/// Entrada do cache de drives: momento da enumeração + snapshot.
type DrivesCache = Option<(Instant, Vec<Drive>)>;

/// Estado da aplicação, compartilhado entre handlers e scheduler.
#[derive(Clone)]
pub struct AppState {
    pub config_path: PathBuf,
    pub db_path: PathBuf,
    /// Conexão de **escrita** compartilhada por todos os writers do daemon
    /// (watcher, scans, backups). Uma só conexão = zero disputa de lock SQLite
    /// dentro do processo ("database is locked" fica estruturalmente impossível —
    /// os writers fazem fila no `Mutex` interno). Leituras podem abrir conexões
    /// próprias: em WAL, leitores nunca bloqueiam writers.
    pub index_db: Arc<IndexDb>,
    pub events: broadcast::Sender<Event>,
    /// Mutex para serializar operações pesadas (scan/tier) — evita concorrência
    /// sobre o SQLite e disks.
    pub run_lock: Arc<AsyncMutex<()>>,
    /// Flag de cancelamento da varredura em andamento (se houver). Setada pelo
    /// endpoint `/api/index/cancel`; o scan a checa e para cedo. Garantimos no máximo
    /// um scan por vez via `run_lock`.
    pub scan_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    /// Cache da enumeração de drives. No Windows a enumeração spawna PowerShell
    /// (`Get-Disk`/`Get-PhysicalDisk`, 1-10s); browse/preview chamam a cada request,
    /// então servimos do cache e renovamos só quando o TTL vence.
    drives_cache: Arc<AsyncMutex<DrivesCache>>,
    /// Watcher em andamento (trocado quando a config muda — ver `api::put_config`).
    pub watcher: Arc<Mutex<Option<crate::watcher::WatcherHandle>>>,
}

impl AppState {
    pub fn new(config_path: PathBuf, db_path: PathBuf) -> anyhow::Result<Self> {
        let (events, _) = broadcast::channel(256);
        // Garante o diretório do banco (um `--db` custom pode apontar p/ pasta
        // inexistente) e abre a conexão de escrita compartilhada.
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let index_db = Arc::new(IndexDb::open(&db_path)?);
        Ok(Self {
            config_path,
            db_path,
            index_db,
            events,
            run_lock: Arc::new(AsyncMutex::new(())),
            scan_cancel: Arc::new(Mutex::new(None)),
            drives_cache: Arc::new(AsyncMutex::new(None)),
            watcher: Arc::new(Mutex::new(None)),
        })
    }

    /// Carrega a config atual (relê do disco a cada chamada).
    pub fn load_config(&self) -> anyhow::Result<Config> {
        Config::load_or_create(&self.config_path)
    }

    /// Drives enumerados com cache (TTL [`DRIVES_TTL`]). A enumeração roda em
    /// `spawn_blocking` — nunca no worker tokio que atende o request.
    pub async fn cached_drives(&self, cfg: &Config) -> anyhow::Result<Vec<Drive>> {
        {
            let guard = self.drives_cache.lock().await;
            if let Some((ts, drives)) = guard.as_ref() {
                if ts.elapsed() < DRIVES_TTL {
                    return Ok(drives.clone());
                }
            }
        }
        let cfg = cfg.clone();
        let drives = tokio::task::spawn_blocking(move || {
            opt_drive_core::drives::enumerate(|m| cfg.tier_for(m))
        })
        .await
        .map_err(|e| anyhow::anyhow!("join da enumeração de drives: {e}"))?;
        *self.drives_cache.lock().await = Some((Instant::now(), drives.clone()));
        Ok(drives)
    }

    pub fn emit(&self, event: Event) {
        // Erro só ocorre se não houver receptores — ignorável.
        let _ = self.events.send(event);
    }
}
