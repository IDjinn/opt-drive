//! Encriptação de arquivos (ChaCha20-Poly1305 + Argon2).
//!
//! Formato `.odenc`: header mágico `ODENC1` + salt Argon2 (16 bytes) + nonce
//! (12 bytes) + ciphertext (com tag AEAD no final). O salt e o nonce são gerados
//! por arquivo — nunca reutilizados. A passphrase nunca é persistida; quem
//! chama lê de variável de ambiente (ver `config::EncryptionConfig`).

use std::io::{Read, Write};
use std::path::Path;

use anyhow::Context;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{AeadCore, ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;

/// Magic + versão do formato.
const MAGIC: &[u8; 6] = b"ODENC1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

/// Deriva a chave de encriptação (256 bits) da passphrase com Argon2id.
fn derive_key(passphrase: &str, salt: &[u8]) -> anyhow::Result<Key> {
    use argon2::Argon2;
    let mut key = Key::default(); // 32 bytes
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("derivar chave com Argon2: {e}"))?;
    Ok(key)
}

/// Encripta `src` para `dst` (arquivo `.odenc`).
pub fn encrypt_file(src: &Path, dst: &Path, passphrase: &str) -> anyhow::Result<()> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let cipher = ChaCha20Poly1305::new(&derive_key(passphrase, &salt)?);

    let plaintext = std::fs::read(src).with_context(|| format!("ler {}", src.display()))?;
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_ref())
        .map_err(|_| anyhow::anyhow!("encriptar {}", src.display()))?;

    let mut out = std::fs::File::create(dst)?;
    out.write_all(MAGIC)?;
    out.write_all(&salt)?;
    out.write_all(&nonce)?;
    out.write_all(&ciphertext)?;
    out.flush()?;
    Ok(())
}

/// Decripta `src` (arquivo `.odenc`) para `dst`.
pub fn decrypt_file(src: &Path, dst: &Path, passphrase: &str) -> anyhow::Result<()> {
    let mut f = std::fs::File::open(src)?;
    let mut header = [0u8; MAGIC.len() + SALT_LEN + NONCE_LEN];
    f.read_exact(&mut header)
        .with_context(|| format!("ler header de {}", src.display()))?;

    let (magic, rest) = header.split_at(MAGIC.len());
    anyhow::ensure!(magic == *MAGIC, "{}: não é um arquivo .odenc", src.display());
    let (salt, nonce_bytes) = rest.split_at(SALT_LEN);

    let cipher = ChaCha20Poly1305::new(&derive_key(passphrase, salt)?);
    let mut ciphertext = Vec::new();
    f.read_to_end(&mut ciphertext)?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext.as_ref())
        .map_err(|_| anyhow::anyhow!("falha ao decriptar (passphrase errada ou arquivo corrompido)"))?;

    std::fs::write(dst, plaintext)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("a.txt");
        std::fs::write(&src, b"conteudo secreto").unwrap();
        let enc = tmp.path().join("a.txt.odenc");
        let dec = tmp.path().join("a.dec.txt");

        encrypt_file(&src, &enc, "senha-correta").unwrap();
        // O ciphertext difere do plaintext e tem o header.
        let raw = std::fs::read(&enc).unwrap();
        assert_eq!(&raw[..6], MAGIC);
        assert_ne!(&raw[34..], b"conteudo secreto");

        decrypt_file(&enc, &dec, "senha-correta").unwrap();
        assert_eq!(std::fs::read(&dec).unwrap(), b"conteudo secreto");
    }

    #[test]
    fn wrong_passphrase_fails() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("a.txt");
        std::fs::write(&src, b"x").unwrap();
        let enc = tmp.path().join("a.txt.odenc");
        encrypt_file(&src, &enc, "certa").unwrap();

        let err = decrypt_file(&enc, &tmp.path().join("out"), "errada").unwrap_err();
        assert!(err.to_string().contains("decriptar"));
    }

    #[test]
    fn rejects_non_odenc_file() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("plain.txt");
        std::fs::write(&src, vec![b'x'; 64]).unwrap();
        let err = decrypt_file(&src, &tmp.path().join("out"), "x").unwrap_err();
        assert!(err.to_string().contains(".odenc"));
    }
}
