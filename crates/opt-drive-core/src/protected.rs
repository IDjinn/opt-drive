//! Caminhos especiais que não podem/devem ser modificados pelo opt-drive.
//!
//! Famílias de proteção:
//! - **Dirs de sistema** filhos diretos da raiz do drive (`C:\Windows`,
//!   `Program Files`, `$RECYCLE.BIN`, …): apagar/mover = máquina inoperante.
//!   Só contam na raiz — um projeto `D:\src\windows` não é protegido.
//! - **Arquivos de sistema** por nome (`pagefile.sys`, `ntuser.dat`, …) em
//!   qualquer nível.
//! - **Pastas de cloud-sync** por nome (`Google Drive`, `OneDrive - …`): apagar
//!   local propaga a exclusão para a nuvem.
//! - **Reparse points** (junctions/symlinks): `remove_dir_all` num link pode
//!   atravessá-lo e apagar o alvo (rede, nuvem, outro drive).
//!
//! Extras do usuário entram via `[[protected_paths]]` na config.

use std::path::{Path, PathBuf};

/// Motivo da proteção (rótulo legível para logs/UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// Dir de sistema na raiz do drive (Windows, Program Files, …).
    SystemDir,
    /// Arquivo de sistema (pagefile.sys, hiberfil.sys, …).
    SystemFile,
    /// Pasta de sincronização de nuvem (Google Drive, OneDrive, …).
    CloudSync,
    /// Caminho listado manualmente pelo usuário na config.
    UserConfigured,
    /// Junction/symlink/reparse point.
    ReparsePoint,
}

impl Protection {
    pub fn label(self) -> &'static str {
        match self {
            Protection::SystemDir => "pasta de sistema",
            Protection::SystemFile => "arquivo de sistema",
            Protection::CloudSync => "pasta de sincronização em nuvem",
            Protection::UserConfigured => "caminho protegido na config",
            Protection::ReparsePoint => "junction/symlink",
        }
    }
}

/// Dirs de sistema (minúsculas) — válidos apenas como filhos diretos da raiz de
/// um drive.
const ROOT_SYSTEM_DIRS: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "$recycle.bin",
    "$windows.~bt",
    "$windows.~ws",
    "system volume information",
    "recovery",
    "config.msi",
    "msocache",
    "perflogs",
];

/// Arquivos de sistema (minúsculos), por nome exato em qualquer nível.
const SYSTEM_FILES: &[&str] = &[
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
    "dumpstack.log.tmp",
    "dumpstack.log",
    "ntuser.dat",
    "ntuser.dat.log",
    "ntuser.dat.log1",
    "ntuser.dat.log2",
    "usrclass.dat",
    "usrclass.dat.log",
];

/// Pastas de cloud-sync (minúsculas): nome exato, ou nome + sufixo (ex.:
/// `OneDrive - Empresa`). Casar em qualquer nível — falso positivo só impede uma
/// ação (falha para o lado seguro).
const CLOUD_DIRS: &[&str] = &["google drive", "googledrive", "onedrive", "dropbox"];

/// O caminho é protegido pelas regras embutidas? Retorna o motivo.
pub fn is_protected(path: &Path) -> Option<Protection> {
    if is_root_system_dir(path) {
        return Some(Protection::SystemDir);
    }
    if is_system_file(path) {
        return Some(Protection::SystemFile);
    }
    if is_cloud_dir(path) {
        return Some(Protection::CloudSync);
    }
    None
}

/// Como [`is_protected`], mas considera também os extras do usuário: um extra
/// protege ele próprio e tudo sob ele (`D:\foo` protege `D:\foo\bar`).
pub fn is_protected_with(path: &Path, extra: &[PathBuf]) -> Option<Protection> {
    if let Some(p) = is_protected(path) {
        return Some(p);
    }
    if extra.iter().any(|e| path.starts_with(e)) {
        return Some(Protection::UserConfigured);
    }
    None
}

/// O caminho é um reparse point (junction/symlink, incl. pastas virtuais de
/// nuvem)? Checa o próprio link — sem seguir. No Windows, `is_symlink` cobre
/// junctions e symlinks.
pub fn is_reparse_point(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Razão pela qual o executor deve recusar operar neste caminho (`None` = ok).
/// Combina as proteções embutidas com o check de reparse point no disco.
pub fn refusal_reason(path: &Path) -> Option<String> {
    if let Some(p) = is_protected(path) {
        return Some(p.label().to_string());
    }
    if is_reparse_point(path) {
        return Some(Protection::ReparsePoint.label().to_string());
    }
    None
}

/// O walker deve pular este caminho (e a subárvore, se dir)? Dirs de sistema na
/// raiz do drive e arquivos de sistema: indexá-los é inútil e caro. Pastas de
/// cloud-sync NÃO são puladas (o índice delas é útil; a proteção fica nas ações).
pub fn skip_in_walk(path: &Path) -> bool {
    is_root_system_dir(path) || is_system_file(path)
}

/// Dir de sistema filho direto da raiz do drive (formato `"X:\"` no parent)?
fn is_root_system_dir(path: &Path) -> bool {
    let Some(name) = file_name_lower(path) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    let parent = parent.to_string_lossy().replace('/', "\\");
    // Raiz de drive Windows: exatamente "X:\" (3 chars, ':' na posição 1).
    let is_drive_root = parent.len() == 3 && parent.as_bytes()[1] == b':';
    is_drive_root && ROOT_SYSTEM_DIRS.contains(&name.as_str())
}

fn is_system_file(path: &Path) -> bool {
    file_name_lower(path)
        .is_some_and(|n| SYSTEM_FILES.contains(&n.as_str()))
}

fn is_cloud_dir(path: &Path) -> bool {
    let Some(name) = file_name_lower(path) else {
        return false;
    };
    CLOUD_DIRS.iter().any(|c| name == *c || name.starts_with(&format!("{c} ")))
}

fn file_name_lower(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_dirs_only_at_drive_root() {
        assert_eq!(
            is_protected(Path::new(r"C:\Windows")),
            Some(Protection::SystemDir)
        );
        assert_eq!(
            is_protected(Path::new(r"c:\program files (x86)")),
            Some(Protection::SystemDir)
        );
        assert_eq!(
            is_protected(Path::new(r"D:\$RECYCLE.BIN")),
            Some(Protection::SystemDir)
        );
        // Case-insensitive.
        assert!(is_protected(Path::new(r"C:\PROGRAMDATA")).is_some());
        // Fora da raiz não protege (projeto legítimo).
        assert_eq!(is_protected(Path::new(r"D:\src\windows")), None);
        assert_eq!(is_protected(Path::new(r"C:\Users\lucas\recovery")), None);
    }

    #[test]
    fn system_files_anywhere() {
        assert_eq!(
            is_protected(Path::new(r"C:\pagefile.sys")),
            Some(Protection::SystemFile)
        );
        assert_eq!(
            is_protected(Path::new(r"C:\dev\proj\ntuser.dat")),
            Some(Protection::SystemFile)
        );
        assert_eq!(is_protected(Path::new(r"C:\dev\proj\pagefile.txt")), None);
    }

    #[test]
    fn cloud_sync_dirs() {
        assert_eq!(
            is_protected(Path::new(r"C:\Users\lucas\Google Drive")),
            Some(Protection::CloudSync)
        );
        assert_eq!(
            is_protected(Path::new(r"C:\Users\lucas\OneDrive - Empresa")),
            Some(Protection::CloudSync)
        );
        assert_eq!(is_protected(Path::new(r"C:\Users\lucas\Dropbox")), Some(Protection::CloudSync));
        assert_eq!(is_protected(Path::new(r"C:\Users\lucas\onedrive-velho")), None);
    }

    #[test]
    fn user_configured_extras_cover_subtree() {
        let extra = vec![PathBuf::from(r"E:\dados")];
        assert_eq!(
            is_protected_with(Path::new(r"E:\dados"), &extra),
            Some(Protection::UserConfigured)
        );
        assert_eq!(
            is_protected_with(Path::new(r"E:\dados\sub\arq.txt"), &extra),
            Some(Protection::UserConfigured)
        );
        assert_eq!(is_protected_with(Path::new(r"E:\outro"), &extra), None);
    }
}
