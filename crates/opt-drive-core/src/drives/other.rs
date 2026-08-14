//! Implementação stub para plataformas não-Windows (Linux/macOS).
//!
//! O foco do projeto é Windows; aqui retornamos uma enumeração simples baseada em
//! pontos de montagem conhecidos sem classificação de bus type.

#![cfg(not(windows))]

use super::{Drive, DriveKind, Tier};

pub fn enumerate_raw() -> Vec<Drive> {
    // Enumeração básica via leitura de /proc/mounts (Linux) — esboço.
    let candidates = ["/", "/home", "/mnt", "/media"];
    candidates
        .iter()
        .filter_map(|m| {
            let path = std::path::Path::new(m);
            let total = std::fs::metadata(path).ok()?;
            let _ = total;
            Some(Drive {
                mount: m.to_string(),
                label: String::new(),
                fs_type: String::new(),
                model: String::new(),
                brand: String::new(),
                total_bytes: 0,
                free_bytes: 0,
                kind: DriveKind::Unknown,
                bus: String::new(),
                rotation_rate: None,
                tier: Tier::Slow,
                read_mbps: None,
                write_mbps: None,
                speed_frac: 0.15,
            })
        })
        .collect()
}
