//! Execução de operações planejadas com segurança (dry-run + journal).
//!
//! O [`Executor`] recebe um [`Plan`] da [`crate::policy`] e o executa ação por ação,
//! escrevendo um [`journal::Journal`] para auditoria. Por padrão roda em modo
//! **dry-run** (apenas planeja); a execução real exige `dry_run = false`.

pub mod cleanup;
pub mod compress;
pub mod encrypt;
pub mod journal;
pub mod relocate;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cleanup_catalog::{CleanupRules, CleanupTarget};
use crate::policy::{Action, Plan};

pub use journal::{Journal, JournalEntry};

/// Executor de planos.
pub struct Executor {
    rules: CleanupRules,
    dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunReport {
    pub dry_run: bool,
    pub relocated: usize,
    pub compressed: usize,
    pub bytes_cleaned: u64,
    pub bytes_moved: u64,
    pub errors: usize,
    pub journal_id: String,
}

impl Executor {
    pub fn new(targets: Vec<CleanupTarget>, dry_run: bool) -> Self {
        let rules =
            CleanupRules::from_targets(&targets).unwrap_or_else(|_| CleanupRules::empty());
        Self { rules, dry_run }
    }

    /// Executa o plano. `progress(desc, frac)` recebe descrição legível + 0..1.
    pub fn execute<F>(
        &self,
        plan: &Plan,
        journal_path: &Path,
        progress: F,
    ) -> anyhow::Result<RunReport>
    where
        F: Fn(&str, f64),
    {
        let mut journal = Journal::new(self.dry_run);
        let mut report = RunReport {
            dry_run: self.dry_run,
            journal_id: journal.id.clone(),
            ..Default::default()
        };

        let total = plan.actions.len().max(1);
        for (i, action) in plan.actions.iter().enumerate() {
            let frac = (i as f64) / (total as f64);
            let (kind, src, dst, junction, _cleanup_deps, compress, size) = match action {
                Action::Relocate {
                    rule,
                    src,
                    dst,
                    junction,
                    cleanup_deps,
                    compress,
                    size_bytes,
                    ..
                } => (
                    format!("relocate ({})", rule),
                    src.clone(),
                    dst.clone(),
                    *junction,
                    *cleanup_deps,
                    *compress,
                    *size_bytes,
                ),
                Action::Compress {
                    rule,
                    src,
                    dst,
                    cleanup_deps,
                    size_bytes,
                } => (
                    format!("compress ({})", rule),
                    src.clone(),
                    dst.clone(),
                    false,
                    *cleanup_deps,
                    false,
                    *size_bytes,
                ),
            };

            progress(&kind, frac);

            if self.dry_run {
                journal.entries.push(JournalEntry {
                    kind,
                    src: src.clone(),
                    dst: dst.clone(),
                    junction,
                    status: "planned".into(),
                    message: Some(format!("{} bytes", size)),
                    undoable: !compress,
                });
                report.bytes_moved += size;
                continue;
            }

            // Execução real. Caminhos protegidos (sistema/nuvem/junctions) são
            // recusados aqui mesmo — rede de segurança caso o plano venha de um
            // índice/config desatualizados.
            let result = match crate::protected::refusal_reason(Path::new(&src)) {
                Some(reason) => Err(anyhow::anyhow!("caminho protegido ({reason}) — ação ignorada")),
                None => self.execute_action(action),
            };
            let (status, msg, undoable) = match result {
                Ok(bytes_cleaned) => {
                    report.bytes_cleaned += bytes_cleaned;
                    report.bytes_moved += size;
                    if matches!(action, Action::Relocate { .. }) {
                        report.relocated += 1;
                    } else {
                        report.compressed += 1;
                    }
                    ("done".to_string(), None, !compress)
                }
                Err(e) => {
                    report.errors += 1;
                    tracing::error!(target = "opt-drive.ops", %src, error = %e, "ação falhou");
                    ("failed".to_string(), Some(format!("{e:#}")), false)
                }
            };
            journal.entries.push(JournalEntry {
                kind,
                src,
                dst,
                junction,
                status,
                message: msg,
                undoable,
            });
        }

        progress("concluído", 1.0);
        journal.save(journal_path)?;
        Ok(report)
    }

    fn execute_action(&self, action: &Action) -> anyhow::Result<u64> {
        let mut cleaned = 0u64;
        match action {
            Action::Relocate {
                src,
                dst,
                junction,
                cleanup_deps,
                ..
            } => {
                let src_p = Path::new(src);
                if *cleanup_deps {
                    let targets = cleanup::collect(src_p, &self.rules);
                    cleaned += cleanup::remove(&targets);
                }
                relocate::move_dir(src_p, Path::new(dst))?;
                if *junction {
                    // recria o caminho original como junction → destino.
                    if let Err(e) = relocate::create_junction(src_p, Path::new(dst)) {
                        tracing::warn!(target = "opt-drive.ops", %src, error = %e,
                            "dados movidos, mas junction não foi criado — caminho original inválido até corrigir");
                    }
                }
            }
            Action::Compress {
                src,
                dst,
                cleanup_deps,
                ..
            } => {
                let src_p = Path::new(src);
                if *cleanup_deps {
                    let targets = cleanup::collect(src_p, &self.rules);
                    cleaned += cleanup::remove(&targets);
                }
                compress::compress_dir(src_p, Path::new(dst), compress::DEFAULT_LEVEL)?;
                // Arquivado: remove o diretório original.
                std::fs::remove_dir_all(src_p).map_err(|e| {
                    anyhow::anyhow!("arquivo criado, mas falhou ao remover origem {src}: {e}")
                })?;
            }
        }
        Ok(cleaned)
    }
}
