//! Catálogo de alvos de limpeza: dependências regeneráveis (node_modules, target,
//! .venv, …) e arquivos "inúteis" (.DS_Store, *.log, *.pyc, …) das principais
//! linguagens, sistema operacional, editores e caches.
//!
//! O catálogo é **extensível**: a config do usuário pode adicionar alvos customizados
//! (em `[cleanup.targets]`) ou desligar o catálogo embutido (`use_default_catalog = false`).

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

/// Categoria de um alvo de limpeza (usada para agrupar na UI/relatórios).
pub const CAT_OS: &str = "os";
pub const CAT_EDITOR: &str = "editor";
pub const CAT_LOGS: &str = "logs";
pub const CAT_CACHE: &str = "cache";

/// Um alvo de limpeza.
///
/// Em `config.toml`, pode ser escrito de forma simples (string) ou detalhada:
///
/// ```toml
/// [[cleanup.targets]]
/// name = "node_modules"
/// patterns = ["node_modules"]
/// category = "javascript"
/// match_dirs = true
/// regenerable = true
///
/// # ou, equivalente resumido:
/// # "node_modules"   (vira match_dirs=true, category="custom")
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupTarget {
    pub name: String,
    /// Globs a casar contra o **nome** do arquivo/diretório (não o caminho completo).
    /// Ex.: `["node_modules"]`, `["*.log", "*.tmp"]`.
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Categoria: linguagem (`"javascript"`, `"rust"`, …) ou `"os"`/`"editor"`/`"logs"`/`"custom"`.
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub description: String,
    /// Aplica-se a diretórios (poda a subárvore).
    #[serde(default = "default_true")]
    pub match_dirs: bool,
    /// Aplica-se a arquivos.
    #[serde(default)]
    pub match_files: bool,
    /// Seguro de apagar sem confirmar (dá pra regenerar). `false` exige cuidado.
    #[serde(default = "default_true")]
    pub regenerable: bool,
}

fn default_category() -> String {
    "custom".into()
}
fn default_true() -> bool {
    true
}

/// Entrada de cleanup na config: aceita tanto uma string simples quanto um objeto
/// detalhado (`#[serde(untagged)]`).
///
/// ```toml
/// [cleanup]
/// targets = ["node_modules"]                  # forma simples
///
/// # ou objeto detalhado:
/// [[cleanup.targets]]
/// name = "logs"
/// patterns = ["*.log"]
/// match_files = true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CleanupEntry {
    Simple(String),
    Target(CleanupTarget),
}

impl CleanupEntry {
    /// Normaliza para `CleanupTarget`.
    pub fn into_target(self) -> CleanupTarget {
        match self {
            CleanupEntry::Simple(s) => CleanupTarget {
                name: s.clone(),
                patterns: vec![s],
                category: "custom".into(),
                description: String::new(),
                match_dirs: true,
                match_files: false,
                regenerable: true,
            },
            CleanupEntry::Target(t) => t,
        }
    }
}

impl CleanupTarget {
    pub(crate) fn dir(name: &str, category: &str) -> Self {
        Self {
            name: name.into(),
            patterns: vec![name.into()],
            category: category.into(),
            description: String::new(),
            match_dirs: true,
            match_files: false,
            regenerable: true,
        }
    }
    pub(crate) fn dirs(name: &str, patterns: &[&str], category: &str) -> Self {
        Self {
            name: name.into(),
            patterns: patterns.iter().map(|s| (*s).into()).collect(),
            category: category.into(),
            description: String::new(),
            match_dirs: true,
            match_files: false,
            regenerable: true,
        }
    }
    pub(crate) fn files(name: &str, patterns: &[&str], category: &str) -> Self {
        Self {
            name: name.into(),
            patterns: patterns.iter().map(|s| (*s).into()).collect(),
            category: category.into(),
            description: String::new(),
            match_dirs: false,
            match_files: true,
            regenerable: true,
        }
    }
}

/// Catálogo embutido abrangente (linguagens principais + junk de OS/editor/logs).
///
/// Alvos de arquivos compilados shippable (*.dll/*.exe/*.so) são propositalmente
/// omitidos para não apagar binários entregues pelo projeto.
pub fn default_catalog() -> Vec<CleanupTarget> {
    vec![
        // --- JavaScript / TypeScript ---
        CleanupTarget::dirs(
            "node_modules",
            &["node_modules", "bower_components"],
            "javascript",
        ),
        CleanupTarget::dirs(
            "js-framework-build",
            &[".next", ".nuxt", ".turbo", ".svelte-kit", ".angular", ".parcel-cache"],
            "javascript",
        ),
        CleanupTarget::dirs("js-dist", &["dist", "build"], "javascript"),
        // --- Rust ---
        CleanupTarget::dir("target", "rust"),
        // --- Python ---
        CleanupTarget::dirs(
            "python-venv",
            &[".venv", "venv", "env", ".env"],
            "python",
        ),
        CleanupTarget::dirs(
            "python-cache",
            &[
                "__pycache__",
                ".pytest_cache",
                ".mypy_cache",
                ".ruff_cache",
                ".tox",
                ".nox",
                "htmlcov",
                ".ipynb_checkpoints",
            ],
            "python",
        ),
        CleanupTarget::files("python-bytecode", &["*.pyc", "*.pyo"], "python"),
        // --- Java / JVM (Maven, Gradle) ---
        CleanupTarget::dirs(
            "jvm-build",
            &["target", "build", "out", ".gradle"],
            "java",
        ),
        CleanupTarget::files("java-class", &["*.class"], "java"),
        // --- C / C++ ---
        CleanupTarget::dirs(
            "cpp-build",
            &[
                "build",
                "cmake-build-debug",
                "cmake-build-release",
                "CMakeFiles",
            ],
            "cpp",
        ),
        CleanupTarget::files("cpp-objects", &["*.o", "*.obj", "*.a"], "cpp"),
        // --- C# / .NET ---
        CleanupTarget::dirs(
            "dotnet-build",
            &["bin", "obj", "packages", "TestResults"],
            "csharp",
        ),
        // --- Swift / Xcode ---
        CleanupTarget::dirs(
            "xcode-build",
            &["DerivedData", ".build", "Pods"],
            "swift",
        ),
        // --- Elixir / Erlang ---
        CleanupTarget::dirs("elixir-build", &["_build", "deps", "cover"], "elixir"),
        // --- Haskell ---
        CleanupTarget::dirs(
            "haskell-build",
            &["dist-newstyle", ".stack-work"],
            "haskell",
        ),
        // --- Dart / Flutter ---
        CleanupTarget::dirs("dart-build", &[".dart_tool"], "dart"),
        // --- Lua ---
        CleanupTarget::dirs("lua-deps", &["lua_modules", ".luarocks"], "lua"),
        // --- R ---
        CleanupTarget::dirs("r-packrat", &["packrat", "renv"], "r"),
        // --- Scala / Metals ---
        CleanupTarget::dirs("scala-metals", &[".metals", ".bloop", "project Metals"], "scala"),
        // --- PHP (composer) ---
        CleanupTarget::dir("php-vendor", "php"),
        // --- Cobertura ---
        CleanupTarget::dirs("coverage", &["coverage", ".nyc_output"], "cache"),
        // --- Sistema operacional ---
        CleanupTarget::files(
            "os-junk",
            &[".DS_Store", "Thumbs.db", "ehthumbs.db", "desktop.ini"],
            CAT_OS,
        ),
        // --- Editores / IDE ---
        CleanupTarget::dirs(
            "ide-config",
            &[".idea", ".vs", ".history"],
            CAT_EDITOR,
        ),
        CleanupTarget::files(
            "editor-swap",
            &["*.swp", "*.swo", "*~", "*.orig", "*.rej"],
            CAT_EDITOR,
        ),
        // --- Logs / temporários ---
        CleanupTarget::files(
            "logs-tmp",
            &["*.log", "*.tmp", "*.bak", "*.pid", "*.lock"],
            CAT_LOGS,
        ),
    ]
}

/// Regras de limpeza compiladas (globsets de diretórios e de arquivos).
///
/// Construído uma vez a partir de `&[CleanupTarget]` e reutilizado pelo walker e
/// pelas operações de limpeza.
pub struct CleanupRules {
    dir_set: Option<GlobSet>,
    file_set: Option<GlobSet>,
}

impl CleanupRules {
    pub fn from_targets(targets: &[CleanupTarget]) -> anyhow::Result<Self> {
        let mut dir_b = GlobSetBuilder::new();
        let mut file_b = GlobSetBuilder::new();
        let mut dir_n = 0usize;
        let mut file_n = 0usize;

        for t in targets {
            for p in &t.patterns {
                let g = match Glob::new(p) {
                    Ok(g) => g,
                    Err(e) => {
                        tracing::warn!(target = "opt-drive.cleanup", pattern = %p, error = %e, "glob inválido, ignorado");
                        continue;
                    }
                };
                if t.match_dirs {
                    dir_b.add(g.clone());
                    dir_n += 1;
                }
                if t.match_files {
                    file_b.add(g);
                    file_n += 1;
                }
            }
        }

        Ok(Self {
            dir_set: if dir_n > 0 { Some(dir_b.build()?) } else { None },
            file_set: if file_n > 0 { Some(file_b.build()?) } else { None },
        })
    }

    /// Diretório casa com algum alvo de diretório?
    pub fn matches_dir(&self, name: &str) -> bool {
        self.dir_set.as_ref().is_some_and(|s| s.is_match(name))
    }

    /// Arquivo casa com algum alvo de arquivo?
    pub fn matches_file(&self, name: &str) -> bool {
        self.file_set.as_ref().is_some_and(|s| s.is_match(name))
    }

    pub fn empty() -> Self {
        Self {
            dir_set: None,
            file_set: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_major_languages() {
        let cat = default_catalog();
        let names: Vec<&str> = cat.iter().map(|t| t.name.as_str()).collect();
        for expected in [
            "node_modules",
            "target",
            "python-venv",
            "jvm-build",
            "cpp-build",
            "dotnet-build",
            "os-junk",
            "logs-tmp",
        ] {
            assert!(names.contains(&expected), "faltando alvo: {expected}");
        }
    }

    #[test]
    fn rules_match_dirs_and_files() {
        let targets = vec![
            CleanupTarget::dir("node_modules", "javascript"),
            CleanupTarget::files("log", &["*.log"], "logs"),
            CleanupTarget::files("ds", &[".DS_Store"], "os"),
        ];
        let rules = CleanupRules::from_targets(&targets).unwrap();

        assert!(rules.matches_dir("node_modules"));
        assert!(!rules.matches_dir("src"));
        assert!(rules.matches_file("error.log"));
        assert!(rules.matches_file(".DS_Store"));
        assert!(!rules.matches_file("main.rs"));
        // node_modules NÃO casa como arquivo, *.log NÃO casa como dir:
        assert!(!rules.matches_file("node_modules"));
        assert!(!rules.matches_dir("error.log"));
    }

    #[test]
    fn default_catalog_compiles() {
        // Garante que todos os globs embutidos são válidos.
        assert!(CleanupRules::from_targets(&default_catalog()).is_ok());
    }
}
