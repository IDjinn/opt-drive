//! Compressão de diretórios em `.tar.zst` (tar + zstd).
//!
//! Usado para o tier `archive`: ao invés de mover solto, empacota o projeto (já
//! limpo de dependências regeneráveis) num único arquivo compactado.

use std::fs::File;
use std::path::Path;

/// Nível de compressão zstd (1=mais rápido .. 22=máximo). Default 3 é um bom meio-termo.
pub const DEFAULT_LEVEL: i32 = 3;

/// Compacta `src` (diretório) em `dst` (ex.: `proj.tar.zst`).
pub fn compress_dir(src: &Path, dst: &Path, level: i32) -> anyhow::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !src.is_dir() {
        anyhow::bail!("comprimir exige um diretório: {}", src.display());
    }

    let file = File::create(dst)?;
    let encoder = zstd::Encoder::new(file, level)?.auto_finish();
    let mut builder = tar::Builder::new(encoder);

    // Nome-base do projeto como top-level do archive.
    let base = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());

    builder.append_dir_all(&base, src)?;
    builder.finish()?;
    Ok(())
}

/// Descomprime um `.tar.zst` de volta para um diretório `dst`.
pub fn decompress_dir(archive: &Path, dst: &Path) -> anyhow::Result<()> {
    let file = File::open(archive)?;
    let decoder = zstd::Decoder::new(file)?;
    let mut ar = tar::Archive::new(decoder);
    std::fs::create_dir_all(dst)?;
    ar.unpack(dst)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_tar_zst() {
        let d = tempdir().unwrap();
        let root = d.path();
        let proj = root.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("a.txt"), "conteudo A").unwrap();
        fs::write(proj.join("b.txt"), "conteudo B bem maior para haver compressao real").unwrap();

        let archive = root.join("proj.tar.zst");
        compress_dir(&proj, &archive, DEFAULT_LEVEL).unwrap();
        assert!(archive.exists());

        let out = root.join("restored");
        decompress_dir(&archive, &out).unwrap();
        // o conteúdo fica sob out/proj
        let restored = out.join("proj");
        assert_eq!(
            fs::read_to_string(restored.join("a.txt")).unwrap(),
            "conteudo A"
        );
    }
}
