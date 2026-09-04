//! Engine de regras: dado um conjunto de [`Rule`]s, o índice e os drives, produz um
//! [`Plan`] de [`Action`]s (mover, comprimir, limpar). Por design, isto é **apenas
//! planejamento** — nada é executado aqui. A execução (com journal/undo) vive em
//! [`crate::ops`].

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use globset::Glob;
use serde::{Deserialize, Serialize};

use crate::drives::{Drive, Tier};
use crate::index::FileEntry;
use crate::usage::ActivityScorer;

/// Pasta base (no drive de destino) para onde os dados tiered vão.
pub const TIERED_DIR: &str = ".opt-drive";

/// Uma regra de tiering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    /// Glob contra o caminho do projeto/pasta (ex.: `**/dev/**`).
    pub match_glob: String,
    /// Mover se inativo há mais de N dias.
    #[serde(default)]
    pub inactive_days: u64,
    /// Tamanho mínimo (em bytes) do alvo para considerar.
    #[serde(default)]
    pub min_size: Option<u64>,
    pub from_tier: Tier,
    pub to_tier: Tier,
    /// Descartar dependências regeneráveis ao mover.
    #[serde(default)]
    pub cleanup_deps: bool,
    /// Criar junction/symlink no caminho original após mover.
    #[serde(default = "default_true")]
    pub junction: bool,
    /// Comprimir em `.tar.zst` (típico quando `to_tier == Archive`).
    #[serde(default)]
    pub compress: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            name: "padrao".into(),
            match_glob: "**/*".into(),
            inactive_days: 30,
            min_size: None,
            from_tier: Tier::Fast,
            to_tier: Tier::Slow,
            cleanup_deps: true,
            junction: true,
            compress: false,
        }
    }
}

/// Ação planejada (ainda não executada).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Action {
    /// Mover pasta entre drives (opcionalmente com junction no caminho original).
    Relocate {
        rule: String,
        src: String,
        dst: String,
        from_drive: String,
        to_drive: String,
        junction: bool,
        cleanup_deps: bool,
        compress: bool,
        size_bytes: u64,
        days_inactive: i64,
    },
    /// Apenas comprimir no lugar (sem mover).
    Compress {
        rule: String,
        src: String,
        dst: String,
        cleanup_deps: bool,
        size_bytes: u64,
    },
}

/// Resultado do planejamento.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub total_bytes_moved: u64,
    pub total_bytes_cleaned_estimate: u64,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

/// Gera um plano a partir das regras, índice e drives.
///
/// `protected` são os extras do usuário (`config.protected_paths`) — os embutidos
/// (sistema/nuvem) vêm de [`crate::protected::is_protected`]; candidatos
/// protegidos nunca geram ações.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    rules: &[Rule],
    entries: &[FileEntry],
    drives: &[Drive],
    scorer: &ActivityScorer,
    now: i64,
    protected: &[std::path::PathBuf],
) -> Plan {
    // Mapa tier → primeiro drive daquele tier (destino padrão).
    let mut drive_by_tier: HashMap<Tier, &Drive> = HashMap::new();
    for d in drives {
        drive_by_tier.entry(d.tier).or_insert(d);
    }

    // Tamanho agregado por project_root.
    let mut size_by_project: HashMap<String, u64> = HashMap::new();
    for e in entries {
        if let Some(root) = &e.project_root {
            *size_by_project.entry(root.clone()).or_default() += e.size;
        }
    }

    // Candidatos: raízes de projeto (is_dir && project_root == próprio caminho).
    let candidates: Vec<&FileEntry> = entries
        .iter()
        .filter(|e| e.is_dir && e.project_root.as_deref() == Some(e.path.as_str()))
        .collect();

    let mut plan_out = Plan::default();
    let mut handled: std::collections::HashSet<String> = std::collections::HashSet::new();

    for rule in rules {
        let glob = match Glob::new(&rule.match_glob) {
            Ok(g) => g.compile_matcher(),
            Err(_) => continue,
        };

        for cand in &candidates {
            if handled.contains(&cand.path) {
                continue;
            }
            // Nunca planejar ações sobre caminhos protegidos (sistema, nuvem,
            // extras da config) — o executor refaz este check antes de agir.
            if crate::protected::is_protected_with(Path::new(&cand.path), protected).is_some() {
                tracing::debug!(
                    target: "opt-drive.policy",
                    path = %cand.path,
                    "skip: caminho protegido"
                );
                continue;
            }
            let debug = std::env::var("OPTDRIVE_DEBUG").is_ok();
            if debug {
                eprintln!(
                    "[opt-drive.policy] cand drive={:?} path={}",
                    cand.drive, cand.path
                );
            }
            if !glob.is_match(&cand.path) && !glob.is_match(Path::new(&cand.path)) {
                if debug {
                    eprintln!("    skip: glob '{}' não casa", rule.match_glob);
                }
                continue;
            }

            // Drive de origem deve ter o tier esperado pela regra.
            let from_drive = match drive_by_tier.get(&rule.from_tier) {
                Some(d) if d.mount == cand.drive => *d,
                other => {
                    if debug {
                        eprintln!(
                            "    skip: drive mismatch (cand={}, from_tier drive={:?})",
                            cand.drive,
                            other.map(|d| d.mount.clone())
                        );
                    }
                    continue;
                }
            };
            let to_drive = match drive_by_tier.get(&rule.to_tier) {
                Some(d) => *d,
                None => continue,
            };
            // Nada a fazer se origem e destino no mesmo drive.
            if from_drive.mount == to_drive.mount {
                continue;
            }

            let days = scorer.days_inactive(cand, now);
            if (days as u64) < rule.inactive_days {
                if debug {
                    eprintln!("    skip: inativo {}d < {}d", days, rule.inactive_days);
                }
                continue;
            }

            let size = size_by_project.get(&cand.path).copied().unwrap_or(0);
            if let Some(min) = rule.min_size {
                if size < min {
                    continue;
                }
            }

            let src_path = Path::new(&cand.path);
            let dst_path = destination_for(src_path, &dest_root_on(to_drive));

            if rule.compress && rule.to_tier == Tier::Archive {
                plan_out.actions.push(Action::Compress {
                    rule: rule.name.clone(),
                    src: cand.path.clone(),
                    dst: format!("{}.tar.zst", dst_path.display()),
                    cleanup_deps: rule.cleanup_deps,
                    size_bytes: size,
                });
                plan_out.total_bytes_moved += size;
            } else {
                plan_out.actions.push(Action::Relocate {
                    rule: rule.name.clone(),
                    src: cand.path.clone(),
                    dst: dst_path.to_string_lossy().into_owned(),
                    from_drive: from_drive.mount.clone(),
                    to_drive: to_drive.mount.clone(),
                    junction: rule.junction,
                    cleanup_deps: rule.cleanup_deps,
                    compress: rule.compress,
                    size_bytes: size,
                    days_inactive: days,
                });
                plan_out.total_bytes_moved += size;
            }
            handled.insert(cand.path.clone());
        }
    }

    plan_out
}

/// Raiz de destino no drive: `<mount>\.opt-drive`.
pub fn dest_root_on(drive: &Drive) -> PathBuf {
    let mut p = PathBuf::from(&drive.mount);
    p.push(TIERED_DIR);
    p
}

/// Reconstrói o caminho relativo (sem prefixo de drive) sob outra raiz.
pub fn destination_for(src: &Path, dest_root: &Path) -> PathBuf {
    let mut out = dest_root.to_path_buf();
    for comp in src.components() {
        if let Component::Normal(name) = comp {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drives::DriveKind;

    fn drive(mount: &str, tier: Tier) -> Drive {
        Drive {
            mount: mount.into(),
            label: String::new(),
            fs_type: "NTFS".into(),
            model: String::new(),
            brand: String::new(),
            total_bytes: 0,
            free_bytes: 0,
            kind: DriveKind::Unknown,
            bus: String::new(),
            rotation_rate: None,
            tier,
            read_mbps: None,
            write_mbps: None,
            speed_frac: 0.15,
        }
    }

    #[test]
    fn plans_relocate_for_inactive_project() {
        let drives = vec![drive("C:\\", Tier::Fast), drive("D:\\", Tier::Slow)];
        let root = "C:\\dev\\proj";
        // projeto com mtime antigo
        let proj = FileEntry {
            path: root.into(),
            is_dir: true,
            size: 0,
            mtime: 0,
            atime: 0,
            drive: "C:\\".into(),
            project_root: Some(root.into()),
        };
        let file = FileEntry {
            path: format!("{}/a.txt", root),
            is_dir: false,
            size: 1000,
            mtime: 0,
            atime: 0,
            drive: "C:\\".into(),
            project_root: Some(root.into()),
        };
        let entries = vec![proj, file];
        let rule = Rule {
            name: "t".into(),
            inactive_days: 1,
            ..Default::default()
        };
        let scorer = ActivityScorer::new();
        let p = plan(&[rule], &entries, &drives, &scorer, 86_400 * 100, &[]);

        assert_eq!(p.actions.len(), 1);
        match &p.actions[0] {
            Action::Relocate {
                dst, junction, cleanup_deps, ..
            } => {
                assert!(dst.starts_with("D:\\.opt-drive"));
                assert!(dst.ends_with("proj"));
                assert!(*junction);
                assert!(*cleanup_deps);
            }
            _ => panic!("esperava Relocate"),
        }
    }

    #[test]
    fn skips_protected_candidates() {
        let drives = vec![drive("C:\\", Tier::Fast), drive("D:\\", Tier::Slow)];
        // Candidato em pasta de cloud-sync + candidato protegido por config.
        let entries = vec![
            FileEntry {
                path: r"C:\Users\lucas\Google Drive".into(),
                is_dir: true,
                size: 0,
                mtime: 0,
                atime: 0,
                drive: "C:\\".into(),
                project_root: Some(r"C:\Users\lucas\Google Drive".into()),
            },
            FileEntry {
                path: r"C:\dev\secreto".into(),
                is_dir: true,
                size: 0,
                mtime: 0,
                atime: 0,
                drive: "C:\\".into(),
                project_root: Some(r"C:\dev\secreto".into()),
            },
        ];
        let rule = Rule {
            name: "t".into(),
            inactive_days: 1,
            ..Default::default()
        };
        let scorer = ActivityScorer::new();
        let protected = vec![std::path::PathBuf::from(r"C:\dev\secreto")];
        let p = plan(&[rule], &entries, &drives, &scorer, 86_400 * 100, &protected);
        assert!(p.actions.is_empty());
    }
}
