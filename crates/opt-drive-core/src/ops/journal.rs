//! Journal: grava um manifest das operações executadas (auditoria + undo parcial).
//!
//! Para cada lote de [`crate::policy::Action`] aplicado, grava-se um arquivo JSON
//! com o que foi feito. Undo é possível para `Relocate` (move de volta + remove
//! junction); `Cleanup` é "regenerável" (não desfazível) e `Compress` desfaz
//! descomprimindo.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub id: String,
    pub created_at: i64,
    pub dry_run: bool,
    pub entries: Vec<JournalEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub kind: String, // "relocate" | "compress"
    pub src: String,
    pub dst: String,
    pub junction: bool,
    pub status: String, // "planned" | "done" | "failed"
    pub message: Option<String>,
    pub undoable: bool,
}

impl Journal {
    pub fn new(dry_run: bool) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: crate::usage::unix_now(),
            dry_run,
            entries: Vec::new(),
        }
    }

    /// Grava o journal como JSON no caminho dado.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(path, raw)?;
        Ok(())
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }
}
