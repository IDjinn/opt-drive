//! Modelo de configuração (TOML).
//!
//! Exemplo:
//! ```toml
//! [[drives]]
//! path = "C:\\"
//! tier = "fast"
//!
//! [[drives]]
//! path = "D:\\"
//! tier = "slow"
//!
//! [watch]
//! paths = ["C:\\dev"]
//! ignore_globs = ["**/.git/**", "**/target/**"]
//!
//! [cleanup]
//! targets = ["node_modules", "target", ".venv", "__pycache__", "dist", "build", ".next"]
//!
//! # Caminhos extras que o opt-drive nunca move/apaga (sistema/nuvem já são
//! # protegidos embutidos — veja `protected.rs`).
//! protected_paths = ["D:\\dados-irreplaceables"]
//!
//! [[rules]]
//! name = "projetos-inativos"
//! match_glob = "**/*"
//! inactive_days = 30
//! from_tier = "fast"
//! to_tier = "slow"
//! cleanup_deps = true
//! junction = true
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cleanup_catalog::{default_catalog, CleanupEntry, CleanupTarget};
use crate::drives::Tier;

/// Configuração raiz.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Atribuição explícita de tiers por ponto de montagem (opcional — o restante é
    /// classificado automaticamente por [`crate::drives`]).
    #[serde(default)]
    pub drives: Vec<DriveConfig>,

    /// O que indexar.
    #[serde(default)]
    pub watch: WatchConfig,

    /// Diretórios regeneráveis a descartar ao mover/comprimir.
    #[serde(default)]
    pub cleanup: CleanupConfig,

    /// Regras de tiering.
    #[serde(default)]
    pub rules: Vec<crate::policy::Rule>,

    /// Ajustes do motor de indexação (paralelismo, uso de CPU).
    #[serde(default)]
    pub indexer: IndexerConfig,

    /// Backup/sync para conectores remotos (S3, Google Drive) ou local.
    #[serde(default)]
    pub backup: BackupConfig,

    /// Caminhos extras que nunca devem ser movidos/apagados (além dos embutidos
    /// em [`crate::protected`]). Um caminho protege toda a sua subárvore.
    #[serde(default)]
    pub protected_paths: Vec<PathBuf>,
}

/// Configuração de backup/sync (Fase 2). `connector` seleciona o backend:
/// `"s3"`, `"google-drive"` ou `"local"` (espelhamento entre drives).
///
/// ```toml
/// [backup]
/// connector = "s3"
/// paths = ["C:\\dev"]
/// schedule_secs = 0
/// delete_remote = false
/// encrypt = false
///
/// [backup.options]
/// bucket = "my-backups"
/// region = "us-east-1"
///
/// [backup.encryption]
/// passphrase_env = "OPT_DRIVE_PASSPHRASE"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupConfig {
    /// Conector a usar: `"s3"`, `"google-drive"` ou `"local"`. Vazio = backup
    /// desabilitado.
    #[serde(default)]
    pub connector: String,
    /// Diretórios a sincronizar.
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    /// Intervalo do job automático no scheduler (0 = só manual). Default: 0.
    #[serde(default)]
    pub schedule_secs: u64,
    /// Apaga no destino o que não existe mais localmente. Default: `false`
    /// (conservador — nunca destrói backup por engano).
    #[serde(default)]
    pub delete_remote: bool,
    /// Encripta arquivos antes do upload (ChaCha20-Poly1305, ver
    /// [`crate::ops::encrypt`]). Default: `false`.
    #[serde(default)]
    pub encrypt: bool,
    /// Opções específicas do conector (chave → valor). Ex.: `bucket`/`region`/
    /// `endpoint` (s3), `credentials_path` (google-drive), `target_root` (local).
    #[serde(default)]
    pub options: std::collections::BTreeMap<String, String>,
    /// Derivação de chave quando `encrypt = true`.
    #[serde(default)]
    pub encryption: EncryptionConfig,
}

/// Onde ler a passphrase de encriptação. A passphrase em si **nunca** fica na
/// config — apenas o nome da variável de ambiente que a contém.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionConfig {
    /// Nome da variável de ambiente com a passphrase. Default:
    /// `OPT_DRIVE_PASSPHRASE`.
    #[serde(default = "default_passphrase_env")]
    pub passphrase_env: String,
}

// `Default` manual (não derivável) porque o campo tem default não-vazio.
#[allow(clippy::derivable_impls)]
impl Default for EncryptionConfig {
    fn default() -> Self {
        Self {
            passphrase_env: default_passphrase_env(),
        }
    }
}

fn default_passphrase_env() -> String {
    "OPT_DRIVE_PASSPHRASE".into()
}

// Manual (não derivável) porque `connector` default é relevante semanticamente
// via `EncryptionConfig`, mantendo o restante idêntico ao derive.
#[allow(clippy::derivable_impls)]
impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            connector: String::new(),
            paths: Vec::new(),
            schedule_secs: 0,
            delete_remote: false,
            encrypt: false,
            options: std::collections::BTreeMap::new(),
            encryption: EncryptionConfig::default(),
        }
    }
}

impl BackupConfig {
    /// Backup habilitado (conector definido e ao menos um caminho).
    pub fn enabled(&self) -> bool {
        !self.connector.is_empty() && !self.paths.is_empty()
    }

    /// Lê a passphrase da variável de ambiente configurada.
    pub fn passphrase(&self) -> anyhow::Result<String> {
        match std::env::var(&self.encryption.passphrase_env) {
            Ok(p) if !p.is_empty() => Ok(p),
            _ => anyhow::bail!(
                "variável de ambiente {} não definida (necessária para encriptação de backup)",
                self.encryption.passphrase_env
            ),
        }
    }
}

/// Configuração do motor de indexação (varredura completa).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexerConfig {
    /// Número de threads de varredura paralela. `0` = **automático** (= nº de CPUs
    /// lógicos). Valores menores limitam o uso de CPU durante o scan — útil para não
    /// competir com tarefas do usuário. Default: `0`.
    #[serde(default)]
    pub threads: usize,
}

impl IndexerConfig {
    /// Resolve o nº efetivo de threads: `0` vira `available_parallelism()`, com fallback
    /// seguro para 1 caso a contagem não esteja disponível.
    pub fn effective_threads(&self) -> usize {
        resolve_threads(self.threads)
    }
}

/// Normaliza um contagem de threads: `0` (ou valor impossível) vira o nº de CPUs; em
/// último caso, 1. Garantimos no mínimo 1 thread.
pub fn resolve_threads(requested: usize) -> usize {
    if requested != 0 {
        return requested.max(1);
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DriveConfig {
    /// Ponto de montagem, ex. `"C:\\"`.
    pub path: String,
    /// Tier imposto pelo usuário.
    pub tier: Tier,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    /// Pastas raiz a varrer.
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    /// Globs a ignorar (além do `.gitignore`).
    #[serde(default)]
    pub ignore_globs: Vec<String>,
    /// Indexação incremental em tempo real via file-watcher (`notify`). Mantém o
    /// índice sincronizado sem re-scan completo. Default: ligado.
    #[serde(default = "default_true")]
    pub realtime: bool,
    /// Janela de debounce (ms) para agrupar bursts de eventos do filesystem
    /// (ex.: `cargo build`/`npm install` tocam milhares de arquivos). Default: 600ms.
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            ignore_globs: vec![
                "**/.git/objects/**".into(),
                "**/target/**".into(),
            ],
            realtime: true,
            debounce_ms: 600,
        }
    }
}

fn default_debounce_ms() -> u64 {
    600
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupConfig {
    /// Inclui o catálogo embutido abrangente (linguagens + OS/editor/logs).
    #[serde(default = "default_true")]
    pub use_default_catalog: bool,
    /// Alvos customizados (string simples OU objeto detalhado). Somam-se ao catálogo.
    #[serde(default)]
    pub targets: Vec<CleanupEntry>,
    /// `name`s de alvos do catálogo embutido a **desligar** (blacklist). Default vazio
    /// (tudo ligado). Permite toggle individual na UI sem perder o catálogo embutido.
    /// Não afeta os alvos custom em `targets`.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Limpa também ao comprimir.
    #[serde(default = "default_true")]
    pub cleanup_on_compress: bool,
}

fn default_true() -> bool {
    true
}

impl CleanupConfig {
    /// Alvos efetivos = (catálogo embutido, se habilitado, menos os `disabled`) + custom.
    pub fn effective_targets(&self) -> Vec<CleanupTarget> {
        let mut out = if self.use_default_catalog {
            default_catalog()
                .into_iter()
                .filter(|t| !self.disabled.iter().any(|d| d == &t.name))
                .collect()
        } else {
            Vec::new()
        };
        for entry in &self.targets {
            out.push(entry.clone().into_target());
        }
        out
    }
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            use_default_catalog: true,
            targets: Vec::new(),
            disabled: Vec::new(),
            cleanup_on_compress: true,
        }
    }
}

impl Config {
    /// Carrega do caminho dado; se não existir, cria um config padrão e o grava.
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            let raw = std::fs::read_to_string(path)?;
            let cfg: Config = toml::from_str(&raw)?;
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save(path)?;
            Ok(cfg)
        }
    }

    /// Grava a config em disco no formato TOML.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        std::fs::write(path, raw)?;
        Ok(())
    }

    /// Resolve o tier de um drive considerando overrides do usuário.
    pub fn tier_for(&self, mount: &str) -> Option<Tier> {
        let normalized = normalize_mount(mount);
        self.drives
            .iter()
            .find(|d| normalize_mount(&d.path) == normalized)
            .map(|d| d.tier)
    }
}

fn normalize_mount(mount: &str) -> String {
    let m = mount.trim_end_matches('\\').trim_end_matches('/');
    m.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips() {
        let cfg = Config::default();
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        // catálogo embutido habilitado por padrão → contém node_modules.
        let names: Vec<String> = back.cleanup.effective_targets().iter().map(|t| t.name.clone()).collect();
        assert!(names.iter().any(|n| n == "node_modules"));
    }

    #[test]
    fn tier_override_lookup() {
        let cfg = Config {
            drives: vec![DriveConfig {
                path: "D:\\".into(),
                tier: Tier::Slow,
            }],
            ..Default::default()
        };
        assert_eq!(cfg.tier_for("D:\\"), Some(Tier::Slow));
        assert_eq!(cfg.tier_for("C:\\"), None);
    }

    #[test]
    fn disabled_targets_are_excluded() {
        let cfg = Config {
            cleanup: CleanupConfig {
                disabled: vec!["node_modules".into(), "target".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let names: Vec<String> = cfg
            .cleanup
            .effective_targets()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        assert!(!names.iter().any(|n| n == "node_modules"));
        assert!(!names.iter().any(|n| n == "target"));
        // Outros alvos do catálogo continuam presentes.
        assert!(names.iter().any(|n| n == "python-venv"));
    }

    #[test]
    fn disabled_round_trips() {
        let cfg = Config {
            cleanup: CleanupConfig {
                disabled: vec!["node_modules".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.cleanup.disabled, vec!["node_modules".to_string()]);
    }

    #[test]
    fn indexer_defaults_and_round_trips() {
        // Config sem a seção [indexer] → default (threads = 0 = automático).
        let raw = r#"
[watch]
paths = []
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.indexer.threads, 0);
        assert!(cfg.indexer.effective_threads() >= 1);

        // Round-trip preserva um valor explícito.
        let cfg = Config {
            indexer: IndexerConfig { threads: 4 },
            ..Default::default()
        };
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.indexer.threads, 4);
        assert_eq!(back.indexer.effective_threads(), 4);
    }

    #[test]
    fn backup_defaults_and_round_trips() {
        // Sem [backup] → desabilitado, com defaults seguros.
        let cfg: Config = toml::from_str("").unwrap();
        assert!(!cfg.backup.enabled());
        assert_eq!(cfg.backup.encryption.passphrase_env, "OPT_DRIVE_PASSPHRASE");

        // Round-trip completo com conector + opções + encriptação.
        let cfg = Config {
            backup: BackupConfig {
                connector: "s3".into(),
                paths: vec!["C:\\dev".into()],
                schedule_secs: 3600,
                delete_remote: false,
                encrypt: true,
                options: [
                    ("bucket".to_string(), "my-backups".to_string()),
                    ("region".to_string(), "us-east-1".to_string()),
                ]
                .into_iter()
                .collect(),
                encryption: EncryptionConfig::default(),
            },
            ..Default::default()
        };
        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert!(back.backup.enabled());
        assert_eq!(back.backup.connector, "s3");
        assert_eq!(back.backup.schedule_secs, 3600);
        assert!(back.backup.encrypt);
        assert_eq!(back.backup.options.get("bucket").unwrap(), "my-backups");
    }
}
