//! Estado compartilhado do daemon + eventos do WebSocket.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::{broadcast, Mutex as AsyncMutex};

use opt_drive_core::config::Config;
use opt_drive_core::ops::RunReport;
use opt_drive_core::policy::Plan;

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
    /// Incremento de indexação em tempo real (file-watcher aplicou um lote).
    IndexUpdated { stats: ChangeStatsDto },
    TierPreview { plan: Plan },
    TierStarted,
    TierProgress { desc: String, frac: f64 },
    TierDone { report: RunReport },
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

/// Estado da aplicação, compartilhado entre handlers e scheduler.
#[derive(Clone)]
pub struct AppState {
    pub config_path: PathBuf,
    pub db_path: PathBuf,
    pub events: broadcast::Sender<Event>,
    /// Mutex para serializar operações pesadas (scan/tier) — evita concorrência
    /// sobre o SQLite e disks.
    pub run_lock: Arc<AsyncMutex<()>>,
    /// Flag de cancelamento da varredura em andamento (se houver). Setada pelo
    /// endpoint `/api/index/cancel`; o scan a checa e para cedo. Garantimos no máximo
    /// um scan por vez via `run_lock`.
    pub scan_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

impl AppState {
    pub fn new(config_path: PathBuf, db_path: PathBuf) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            config_path,
            db_path,
            events,
            run_lock: Arc::new(AsyncMutex::new(())),
            scan_cancel: Arc::new(Mutex::new(None)),
        }
    }

    /// Carrega a config atual (relê do disco a cada chamada).
    pub fn load_config(&self) -> anyhow::Result<Config> {
        Config::load_or_create(&self.config_path)
    }

    pub fn emit(&self, event: Event) {
        // Erro só ocorre se não houver receptores — ignorável.
        let _ = self.events.send(event);
    }
}
