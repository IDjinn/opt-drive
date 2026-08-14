//! opt-drive CLI — automação/manual sobre o core.
//!
//! Uso:
//!   opt-drive init              cria config padrão
//!   opt-drive drives            lista drives + tiers classificados
//!   opt-drive scan              indexa os watch paths
//!   opt-drive status            estatísticas do índice
//!   opt-drive tier              PREVIEW (dry-run) do tiering
//!   opt-drive tier --apply      executa o tiering de fato
//!   opt-drive config show       mostra config atual

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use opt_drive_core::{
    config::Config,
    drives,
    index::{IndexDb, Indexer},
    ops::Executor,
    policy,
    usage::{unix_now, ActivityScorer},
};

#[derive(Parser)]
#[command(
    name = "opt-drive",
    version,
    about = "Gerenciamento inteligente de arquivos/backups com tiering entre drives"
)]
struct Cli {
    /// Caminho alternativo para o arquivo de config.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Caminho alternativo para o banco de índice.
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Cria o arquivo de config padrão (se inexistente) e mostra o caminho.
    Init,
    /// Lista os drives montados e seus tiers.
    Drives,
    /// Varre os watch paths e (re)constrói o índice.
    Scan,
    /// Estatísticas do índice atual.
    Status,
    /// Planeia (preview) ou aplica o tiering.
    Tier {
        /// Executa de fato (default = apenas preview/dry-run).
        #[arg(long)]
        apply: bool,
    },
    /// Operações de configuração.
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Mostra o caminho e o conteúdo da config atual.
    Show,
    /// Abre a config no editor padrão (usa $EDITOR, fallback notepad).
    Edit,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // `opt_drive` = targets derivados do nome do crate (underscores);
                // `opt-drive` = alvos explícitos pela convenção do AGENTS.md (hífens).
                .unwrap_or_else(|_| "opt_drive=info,opt-drive=info,warn".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(opt_drive_core::default_config_path);
    let db_path = cli.db.clone().unwrap_or_else(opt_drive_core::default_db_path);

    match cli.command {
        Cmd::Init => cmd_init(&config_path),
        Cmd::Drives => cmd_drives(&config_path),
        Cmd::Scan => cmd_scan(&config_path, &db_path),
        Cmd::Status => cmd_status(&db_path),
        Cmd::Tier { apply } => cmd_tier(&config_path, &db_path, apply),
        Cmd::Config { action } => match action {
            ConfigCmd::Show => cmd_config_show(&config_path),
            ConfigCmd::Edit => cmd_config_edit(&config_path),
        },
    }
}

fn load_config(config_path: &Path) -> anyhow::Result<Config> {
    Config::load_or_create(config_path)
}

fn cmd_init(config_path: &Path) -> anyhow::Result<()> {
    if config_path.exists() {
        println!("Config já existe em: {}", config_path.display());
    } else {
        Config::default().save(config_path)?;
        println!("Config padrão criado em: {}", config_path.display());
    }
    Ok(())
}

fn cmd_drives(config_path: &Path) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let drives = drives::enumerate(|m| cfg.tier_for(m));

    if drives.is_empty() {
        println!("Nenhum drive detectado.");
        return Ok(());
    }

    println!(
        "{:<6} {:<22} {:<7} {:<7} {:<7} {:>8} {:>10}",
        "DRIVE", "MODELO", "KIND", "TIER", "BUS", "USADO", "LIVRE"
    );
    println!("{}", "-".repeat(80));
    for d in &drives {
        let name = if d.label.is_empty() {
            d.model.clone()
        } else {
            format!("{} ({})", d.model, d.label)
        };
        println!(
            "{:<6} {:<22} {:<7} {:<7} {:<7} {:>7}% {:>10}",
            d.mount,
            truncate(&name, 22),
            d.kind.as_str(),
            format!("{:?}", d.tier).to_lowercase(),
            truncate(&d.bus, 7),
            format!("{:.0}", d.used_pct()),
            format_bytes(d.free_bytes),
        );
    }
    Ok(())
}

fn cmd_scan(config_path: &Path, db_path: &Path) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    if cfg.watch.paths.is_empty() {
        println!("Nenhum watch path configurado. Edite a config:");
        println!("  {}", config_path.display());
        println!("e adicione caminhos em [watch].paths.");
        return Ok(());
    }

    let indexer = Indexer::open(db_path)?;
    let cleanup = cfg.cleanup.effective_targets();
    let threads = cfg.indexer.threads;

    // Barra de progresso em tempo real (spinner + contador de entradas + pasta atual).
    let pb = indicatif::ProgressBar::new_spinner();
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} {elapsed_precise} {msg}",
        )
        .unwrap()
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb.set_message("indexando…");

    let stats = indexer.scan(
        &cfg.watch.paths,
        &cfg.watch.ignore_globs,
        &cleanup,
        threads,
        None,
        |p| {
            let dir = match p.current_dir.as_deref() {
                Some(d) if !d.is_empty() => format!(" · {d}"),
                _ => String::new(),
            };
            pb.set_message(format!("{} entradas{dir}", p.indexed));
        },
    )?;

    pb.finish_with_message(format!(
        "✓ {} raízes · {} entradas ({} dirs, {} arquivos) · {}",
        stats.roots_scanned,
        stats.entries_indexed,
        stats.dirs,
        stats.files,
        format_bytes(stats.total_bytes),
    ));
    Ok(())
}

fn cmd_status(db_path: &Path) -> anyhow::Result<()> {
    let db = IndexDb::open(db_path)?;
    let count = db.count();
    let last = db.last_indexed();
    println!("Banco:    {}", db_path.display());
    println!("Entradas: {}", count);
    if let Some(ts) = last {
        println!("Última indexação: {} (unix)", ts);
    } else {
        println!("Última indexação: (nenhuma — rode 'opt-drive scan')");
    }
    Ok(())
}

fn cmd_tier(config_path: &Path, db_path: &Path, apply: bool) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let drives = drives::enumerate(|m| cfg.tier_for(m));

    let db = IndexDb::open(db_path)?;
    if db.count() == 0 {
        println!("Índice vazio — rode 'opt-drive scan' primeiro.");
        return Ok(());
    }
    let entries = db.list_all();
    let scorer = ActivityScorer::new();
    let plan = policy::plan(&cfg.rules, &entries, &drives, &scorer, unix_now());

    if plan.is_empty() {
        println!("Nenhuma ação de tiering a fazer. 🎯");
        return Ok(());
    }

    println!(
        "\n{} TIERING {} — {} ação(ões), ~{} a mover\n",
        if apply { "▶ APLICAR" } else { "👁 PREVIEW" },
        if apply { "" } else { "(dry-run)" },
        plan.actions.len(),
        format_bytes(plan.total_bytes_moved),
    );

    for a in &plan.actions {
        match a {
            policy::Action::Relocate {
                rule,
                src,
                dst,
                junction,
                cleanup_deps,
                size_bytes,
                days_inactive,
                ..
            } => {
                println!("  ↪ [{}] {}", rule, src);
                println!("      → {}", dst);
                println!(
                    "      {} | inativo {}d | junction={} | cleanup={}",
                    format_bytes(*size_bytes),
                    days_inactive,
                    junction,
                    cleanup_deps,
                );
            }
            policy::Action::Compress {
                rule, src, dst, size_bytes, ..
            } => {
                println!("  📦 [{}] {}", rule, src);
                println!("      → {}", dst);
                println!("      {}", format_bytes(*size_bytes));
            }
        }
    }

    if !apply {
        println!("\n(preview) Para executar: opt-drive tier --apply");
        return Ok(());
    }

    let journal_dir = data_journals_dir();
    std::fs::create_dir_all(&journal_dir)?;
    let journal_path = journal_dir.join(format!("{}.json", chronoish_id()));
    let exec = Executor::new(cfg.cleanup.effective_targets(), false);
    let report = exec.execute(&plan, &journal_path, |desc, frac| {
        eprintln!("[{:5.1}%] {}", frac * 100.0, desc);
    })?;
    println!(
        "\n✅ Feito: {} movidos, {} comprimidos, ~{} limpos, {} erros.",
        report.relocated,
        report.compressed,
        format_bytes(report.bytes_cleaned),
        report.errors,
    );
    println!("Journal: {}", journal_path.display());
    Ok(())
}

fn cmd_config_show(config_path: &Path) -> anyhow::Result<()> {
    println!("Caminho: {}", config_path.display());
    println!("----------------------------------------");
    let raw = if config_path.exists() {
        std::fs::read_to_string(config_path)?
    } else {
        toml::to_string_pretty(&Config::default())?
    };
    println!("{}", raw);
    Ok(())
}

fn cmd_config_edit(config_path: &Path) -> anyhow::Result<()> {
    if !config_path.exists() {
        Config::default().save(config_path)?;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "notepad".to_string());
    let status = std::process::Command::new(&editor)
        .arg(config_path)
        .status()?;
    if !status.success() {
        anyhow::bail!("editor terminou com erro");
    }
    Ok(())
}

// --- helpers ------------------------------------------------------------------

fn format_bytes(b: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    if b >= TB {
        format!("{:.2} TB", b as f64 / TB as f64)
    } else if b >= GB {
        format!("{:.2} GB", b as f64 / GB as f64)
    } else if b >= MB {
        format!("{:.2} MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.1} KB", b as f64 / KB as f64)
    } else {
        format!("{} B", b)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

fn data_journals_dir() -> PathBuf {
    let proj = directories::ProjectDirs::from("dev", "optdrive", "opt-drive")
        .expect("dir de dados do SO");
    proj.data_dir().join("journals")
}

fn chronoish_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:x}", secs)
}
