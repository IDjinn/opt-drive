//! Movimentação de diretórios entre drives + tiering transparente via junction.
//!
//! - **Mover**: `std::fs::rename` falha entre volumes (EXDEV). No Windows usamos
//!   `robocopy /MOVE` (robusto para árvores grandes); em outros SOs, copy+delete.
//! - **Junction** (Windows): recria o caminho original apontando para o destino no
//!   drive lento — assim `C:\dev\proj` continua funcionando após a mudança.

use std::path::{Path, PathBuf};

/// Move um diretório de `src` para `dst` (possivelmente entre drives).
pub fn move_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    #[cfg(windows)]
    {
        move_dir_robocopy(src, dst)
    }
    #[cfg(not(windows))]
    {
        move_dir_copy_delete(src, dst)
    }
}

#[cfg(windows)]
fn move_dir_robocopy(src: &Path, dst: &Path) -> anyhow::Result<()> {
    use std::process::Command;

    // robocopy <src> <dst> /E /MOVE /NFL /NDL /NJH /NJS /NP
    // /E    copia subdirs inclusive vazios
    // /MOVE move (apaga origem após cópia)
    let src_s = src.to_string_lossy().into_owned();
    let dst_s = dst.to_string_lossy().into_owned();
    let status = Command::new("robocopy")
        .arg(&src_s)
        .arg(&dst_s)
        .args(["/E", "/MOVE", "/NFL", "/NDL", "/NJH", "/NJS", "/NP"])
        .status()?;

    // robocopy: códigos de saída < 8 são sucesso.
    let code = status.code().unwrap_or(1);
    if code >= 8 {
        anyhow::bail!("robocopy falhou com código de saída {code}");
    }
    Ok(())
}

#[cfg(not(windows))]
fn move_dir_copy_delete(src: &Path, dst: &Path) -> anyhow::Result<()> {
    copy_dir_all(src, dst)?;
    std::fs::remove_dir_all(src)?;
    Ok(())
}

#[allow(dead_code)]
fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn create_junction(link: &Path, target: &Path) -> anyhow::Result<()> {
    use std::process::Command;
    if link.exists() {
        // remove handler: se já existe (ex.: junction antigo), remove antes.
        let _ = std::fs::remove_dir(link);
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // mklink é builtin do cmd: cmd /c mklink /J <link> <target>
    let out = Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &link.to_string_lossy(),
            &target.to_string_lossy(),
        ])
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("falha ao criar junction: {err}");
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn create_junction(link: &Path, target: &Path) -> anyhow::Result<()> {
    // Em Unix, symlink cai como aproximação.
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

/// Remove um junction/symlink sem tocar no destino.
pub fn remove_junction(path: &Path) -> anyhow::Result<()> {
    // `remove_dir` remove o link (junction/symlink de diretório), não o alvo.
    std::fs::remove_dir(path)?;
    Ok(())
}

/// Caminho absoluto "canônico" do destino (resolve . e ..) — útil p/ validar junction.
pub fn canonicalize_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}
