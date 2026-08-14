//! Enumeração e classificação de drives em tiers.
//!
//! A classificação combina o *bus type* (NVMe vs SATA vs USB, via
//! `IOCTL_STORAGE_QUERY_PROPERTY`) com o *seek penalty* (rotacional vs não).
//!
//! - NVMe → [`Tier::Fast`]
//! - SSD (SATA, sem seek penalty) → [`Tier::Slow`]
//! - HDD (com seek penalty) ou removível → [`Tier::Archive`]

use serde::{Deserialize, Serialize};

#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

#[cfg(not(windows))]
#[path = "other.rs"]
mod platform;

/// Tier de desempenho atribuído a um drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Mais rápido (ex.: NVMe).
    Fast,
    /// Intermediário (ex.: SSD SATA).
    #[default]
    Slow,
    /// Armazenamento frio (HDD / removível / comprimido).
    Archive,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Fast => "fast (NVMe)",
            Tier::Slow => "slow (SSD)",
            Tier::Archive => "archive (HDD/cold)",
        }
    }

    /// Ordem relativo de "rapidez": Fast > Slow > Archive.
    pub fn rank(self) -> u8 {
        match self {
            Tier::Fast => 2,
            Tier::Slow => 1,
            Tier::Archive => 0,
        }
    }
}

/// Natureza física do dispositivo (best-effort).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DriveKind {
    Nvme,
    Ssd,
    Hdd,
    Removable,
    #[default]
    Unknown,
}

impl DriveKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DriveKind::Nvme => "nvme",
            DriveKind::Ssd => "ssd",
            DriveKind::Hdd => "hdd",
            DriveKind::Removable => "removable",
            DriveKind::Unknown => "unknown",
        }
    }
}

/// Drive detectado.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drive {
    /// Ponto de montagem, ex. `"C:\\"`.
    pub mount: String,
    /// Rótulo do volume.
    pub label: String,
    /// Sistema de arquivos, ex. `"NTFS"`.
    pub fs_type: String,
    /// Modelo amigável do disco físico, ex. `"Samsung SSD 980"`.
    pub model: String,
    /// Fabricante (best-effort), ex. `"Samsung"`. Vazio se não for possível determinar.
    #[serde(default)]
    pub brand: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub kind: DriveKind,
    /// Bus type textual, ex. `"NVMe"`, `"SATA"`, `"USB"`.
    pub bus: String,
    /// RPM informado (0/None quando não-rotacional ou indisponível).
    pub rotation_rate: Option<u32>,
    /// Tier classificado (já considerando overrides de config, se aplicados).
    pub tier: Tier,
    /// Estimativa de leitura sequencial (MB/s). `None` quando indeterminado.
    #[serde(default)]
    pub read_mbps: Option<u64>,
    /// Estimativa de escrita sequencial (MB/s). `None` quando indeterminado.
    #[serde(default)]
    pub write_mbps: Option<u64>,
    /// Score 0..1 de desempenho relativo (alimenta o gauge na UI).
    /// Mapeamento não-linear para diferenciar visualmente NVMe/SSD/HDD.
    #[serde(default)]
    pub speed_frac: f32,
}

impl Drive {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    pub fn used_pct(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.used_bytes() as f64 / self.total_bytes as f64) * 100.0
        }
    }
}

/// Enumera os drives fixos/removíveis montados, classificando o tier automaticamente.
///
/// `tier_override` permite aplicar os overrides de config (mapeia mount → Tier).
pub fn enumerate<F>(tier_override: F) -> Vec<Drive>
where
    F: Fn(&str) -> Option<Tier>,
{
    let mut drives = platform::enumerate_raw();
    for d in &mut drives {
        if let Some(t) = tier_override(&d.mount) {
            d.tier = t;
        }
    }
    drives
}

/// Conveniência: enumera sem overrides (classificação 100% automática).
pub fn enumerate_auto() -> Vec<Drive> {
    enumerate(|_| None)
}

/// Encontra o ponto de montagem (raiz) que contém um caminho qualquer, normalizado
/// para o mesmo formato usado por [`enumerate`] (ex. `"C:\\"`).
pub fn mount_of(path: &std::path::Path) -> Option<String> {
    use std::path::{Component, Prefix};
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    for comp in abs.components() {
        if let Component::Prefix(p) = comp {
            return match p.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    Some(format!("{}:\\", letter as char))
                }
                _ => None,
            };
        }
    }
    // Sem prefixo de drive (ex.: Unix) — retorna a raiz "/".
    abs.ancestors()
        .last()
        .map(|root| root.to_string_lossy().into_owned())
}

/// Classifica um drive a partir do tipo e do seek penalty.
pub(crate) fn classify(kind: DriveKind, has_seek_penalty: bool, removable: bool) -> Tier {
    if removable {
        return Tier::Archive;
    }
    match kind {
        DriveKind::Nvme => Tier::Fast,
        DriveKind::Ssd => Tier::Slow,
        DriveKind::Hdd => Tier::Archive,
        // fallback heurístico: sem seek penalty = sólido; com = rotacional
        DriveKind::Removable => Tier::Archive,
        DriveKind::Unknown => {
            if has_seek_penalty {
                Tier::Archive
            } else {
                Tier::Slow
            }
        }
    }
}

/// Estimativa de throughput sequencial (MB/s) + score relativo (0..1).
///
/// Valores típicos por natureza do dispositivo — não são medidos, são aproximações
/// para alimentar o gauge e a tooltip da UI. O score é não-linear (e não linear a
/// MB/s) para que SSD/HDD fiquem visualmente distintos de NVMe.
pub(crate) fn estimate_speed(kind: DriveKind, rotation: Option<u32>) -> (Option<u64>, Option<u64>, f32) {
    match kind {
        DriveKind::Nvme => (Some(3500), Some(2500), 1.0),
        DriveKind::Ssd => (Some(550), Some(520), 0.55),
        DriveKind::Hdd => {
            // 7200 rpm é o default quando a rotação é desconhecida.
            let rpm = rotation.unwrap_or(7200);
            if rpm >= 7200 {
                (Some(200), Some(180), 0.25)
            } else {
                (Some(150), Some(130), 0.18)
            }
        }
        DriveKind::Removable => (Some(400), Some(400), 0.35),
        DriveKind::Unknown => (None, None, 0.15),
    }
}

/// Determina o fabricante a partir do `Manufacturer` (PowerShell) e/ou do modelo.
///
/// Prioriza `manufacturer` quando ele próprio é uma marca reconhecível; caso
/// contrário faz match por prefixo/substring no `model` (ex.: `"WDC WD..."` →
/// Western Digital, `"ST..."` → Seagate). Retorna `""` se nada casar.
pub(crate) fn brand_of(model: &str, manufacturer: &str) -> String {
    // (substring case-insensitive, nome canônico). Ordem releva só para legibilidade.
    const BRANDS: &[(&str, &str)] = &[
        ("samsung", "Samsung"),
        ("western digital", "Western Digital"),
        ("wdc", "Western Digital"),
        ("wd ", "Western Digital"),
        ("seagate", "Seagate"),
        // "ST" é o prefixo de modelo da Seagate — só casa no início.
        ("crucial", "Crucial"),
        ("kingston", "Kingston"),
        ("sandisk", "SanDisk"),
        ("intel", "Intel"),
        ("kioxia", "Kioxia"),
        ("micron", "Micron"),
        ("toshiba", "Toshiba"),
        ("sk hynix", "SK Hynix"),
        ("hbg3", "SK Hynix"),
        ("pny", "PNY"),
        ("adata", "ADATA"),
        ("a-data", "ADATA"),
        ("corsair", "Corsair"),
        ("plextor", "Plextor"),
        ("lenovo", "Lenovo"),
        ("hitachi", "Hitachi"),
        ("hgst", "HGST"),
        ("maxtor", "Maxtor"),
        ("wd elements", "Western Digital"),
        ("msi", "MSI"),
        ("gigabyte", "Gigabyte"),
        ("sabrent", "Sabrent"),
        ("lexar", "Lexar"),
        ("silicon power", "Silicon Power"),
        ("teamgroup", "TeamGroup"),
        ("team ", "TeamGroup"),
    ];

    let canon = |s: &str| -> Option<&'static str> {
        let lower = s.to_lowercase();
        // Caso especial: prefixo "ST" no início do modelo → Seagate.
        if lower.starts_with("st") && lower.len() >= 3 {
            // "ST" seguido de dígitos (ex.: ST1000) → Seagate. Evita falsos como "storage".
            let after = lower.chars().nth(2).unwrap_or(' ');
            if after.is_ascii_digit() {
                return Some("Seagate");
            }
        }
        // Caso especial: "CT" no início → Crucial (CT500MX etc.).
        if lower.starts_with("ct") && lower.len() >= 3 {
            let after = lower.chars().nth(2).unwrap_or(' ');
            if after.is_ascii_digit() {
                return Some("Crucial");
            }
        }
        BRANDS
            .iter()
            .find(|(pat, _)| lower.contains(pat))
            .map(|(_, name)| *name)
    };

    // 1) Manufacturer, se for uma marca reconhecível (PowerShell às vezes devolve
    //    genéricos como "(Standard disk drives)" — esses não casam e são ignorados).
    if let Some(b) = canon(manufacturer) {
        return b.to_string();
    }
    // 2) Caso contrário, tenta pelo modelo.
    if let Some(b) = canon(model) {
        return b.to_string();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_speed_covers_kinds() {
        let (r, w, f) = estimate_speed(DriveKind::Nvme, None);
        assert_eq!(r, Some(3500));
        assert_eq!(w, Some(2500));
        assert!((f - 1.0).abs() < 1e-6);

        let (r, _w, f) = estimate_speed(DriveKind::Ssd, None);
        assert_eq!(r, Some(550));
        assert!((f - 0.55).abs() < 1e-6);

        // HDD 7200 (default).
        let (r, _, f) = estimate_speed(DriveKind::Hdd, None);
        assert_eq!(r, Some(200));
        assert!((f - 0.25).abs() < 1e-6);

        // HDD 5400.
        let (r, _, f) = estimate_speed(DriveKind::Hdd, Some(5400));
        assert_eq!(r, Some(150));
        assert!((f - 0.18).abs() < 1e-6);

        // Unknown → None.
        let (r, w, _) = estimate_speed(DriveKind::Unknown, None);
        assert_eq!(r, None);
        assert_eq!(w, None);
    }

    #[test]
    fn brand_from_manufacturer_or_model() {
        // Manufacturer reconhecido tem prioridade.
        assert_eq!(brand_of("Samsung SSD 980", "Samsung"), "Samsung");
        // Manufacturer genérico ignorado → cai no modelo.
        assert_eq!(brand_of("Samsung SSD 980", "(Standard disk drives)"), "Samsung");
        // Prefixo de modelo Seagate (ST + dígitos).
        assert_eq!(brand_of("ST1000DM003-1ER162", ""), "Seagate");
        // Crucial por prefixo CT.
        assert_eq!(brand_of("CT500P5SSD8", ""), "Crucial");
        // WDC → Western Digital.
        assert_eq!(brand_of("WDC WD10EZEX-00WN4A0", ""), "Western Digital");
        // Desconhecido.
        assert_eq!(brand_of("Disco Genérico XYZ", "OEM Desconhecido"), "");
    }
}
