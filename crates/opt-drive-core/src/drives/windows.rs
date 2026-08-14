//! Implementação Windows.
//!
//! - **Espaço/rótulo/FS** via Win32 (`GetLogicalDriveStringsW`, `GetVolumeInformationW`,
//!   `GetDiskFreeSpaceExW`) — funcionam sem privilégios.
//! - **BusType / MediaType (SSD/NVMe/HDD)** via PowerShell `Get-Disk`/`Get-PhysicalDisk`.
//!   Abrir `\\.\C:` com `IOCTL_STORAGE_QUERY_PROPERTY` exige admin (ERROR_ACCESS_DENIED);
//!   o cmdlet Storage expõe os mesmos dados sem elevation.

#![cfg(windows)]

use std::collections::HashMap;

use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW, GetVolumeInformationW,
};

use super::{classify, Drive, DriveKind};

const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOVABLE: u32 = 2;

/// Informação de hardware por letra de unidade.
struct DiskPhys {
    bus: String,
    media: String,
    friendly: String,
    manufacturer: String,
    spindle: Option<u32>,
}

/// Ponto de entrada pública do módulo de plataforma.
pub fn enumerate_raw() -> Vec<Drive> {
    let drive_strings = match logical_drive_strings() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let phys = powershell_disks();

    let mut drives = Vec::new();
    for mount in drive_strings.split('\0').filter(|s| !s.is_empty()) {
        let drive_type = unsafe { GetDriveTypeW(enc_wide(mount).as_ptr()) };
        let removable = drive_type == DRIVE_REMOVABLE;
        if drive_type != DRIVE_FIXED && drive_type != DRIVE_REMOVABLE {
            continue;
        }

        let (label, fs_type) = volume_info(mount);
        let (total, free) = disk_space(mount);

        let letter = mount.chars().next().unwrap_or('\0');
        let p = phys.get(&letter);
        let (kind, bus, rotation, has_seek_penalty) = classify_phys(p, removable);
        let model = p.map(|x| x.friendly.clone()).unwrap_or_default();
        let manufacturer = p.map(|x| x.manufacturer.clone()).unwrap_or_default();
        let brand = super::brand_of(&model, &manufacturer);
        let (read_mbps, write_mbps, speed_frac) = super::estimate_speed(kind, rotation);

        let tier = classify(kind, has_seek_penalty, removable);

        drives.push(Drive {
            mount: mount.to_string(),
            label,
            fs_type,
            model,
            brand,
            total_bytes: total,
            free_bytes: free,
            kind,
            bus,
            rotation_rate: rotation,
            tier,
            read_mbps,
            write_mbps,
            speed_frac,
        });
    }
    drives
}

fn classify_phys(p: Option<&DiskPhys>, removable: bool) -> (DriveKind, String, Option<u32>, bool) {
    let Some(p) = p else {
        return (DriveKind::Unknown, String::new(), None, removable);
    };

    let bus_lc = p.bus.to_lowercase();
    let media_lc = p.media.to_lowercase();

    let kind = if bus_lc.contains("nvme") {
        DriveKind::Nvme
    } else if bus_lc.contains("usb") || removable {
        DriveKind::Removable
    } else if media_lc == "hdd" {
        DriveKind::Hdd
    } else if media_lc == "ssd" || media_lc == "scm" || media_lc == "unspecified" {
        // "unspecified" mas não HDD → assume SSD (comum em NVMe onde MediaType fica vazio).
        DriveKind::Ssd
    } else {
        DriveKind::Unknown
    };

    let has_seek_penalty = matches!(kind, DriveKind::Hdd);
    // RPM real do Get-PhysicalDisk quando disponível (HDD); fallback 7200.
    let rotation = if has_seek_penalty {
        Some(p.spindle.unwrap_or(7200))
    } else {
        None
    };
    let bus = if p.bus.is_empty() {
        kind.as_str().to_string()
    } else {
        p.bus.clone()
    };
    (kind, bus, rotation, has_seek_penalty)
}

/// Executa `Get-PhysicalDisk`/`Get-Disk` via PowerShell e mapeia letra → dados.
fn powershell_disks() -> HashMap<char, DiskPhys> {
    let cmd = r#"
$ErrorActionPreference='SilentlyContinue'
Get-Disk | ForEach-Object {
  $d=$_
  $letters=((Get-Partition -DiskNumber $d.Number).DriveLetter | Where-Object {$_})
  $phys=(Get-PhysicalDisk | Where-Object { $_.DeviceId -eq ([string]$d.Number) })
  [PSCustomObject]@{
    BusType  = [string]$d.BusType
    MediaType= if($phys){[string]$phys.MediaType}else{''}
    Friendly = [string]$d.FriendlyName
    Manufacturer = [string]$d.Manufacturer
    SpindleSpeed = if($phys){[string]$phys.SpindleSpeed}else{''}
    Letters  = (($letters) -join ',')
  }
} | ConvertTo-Json -Compress
"#;

    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            cmd,
        ])
        .output();

    let out = match output {
        Ok(o) if o.status.success() => o,
        _ => return HashMap::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    if text.is_empty() {
        return HashMap::new();
    }

    // ConvertTo-Json devolve um objeto (1 disco) ou array (vários). Normaliza p/ array.
    let normalized = if text.starts_with('[') {
        text.to_string()
    } else {
        format!("[{}]", text)
    };

    #[derive(serde::Deserialize)]
    struct DiskJson {
        #[serde(rename = "BusType", default)]
        bus_type: String,
        #[serde(rename = "MediaType", default)]
        media_type: String,
        #[serde(rename = "Friendly", default)]
        friendly: String,
        #[serde(rename = "Manufacturer", default)]
        manufacturer: String,
        #[serde(rename = "SpindleSpeed", default)]
        spindle_speed: String,
        #[serde(rename = "Letters", default)]
        letters: String,
    }

    let disks: Vec<DiskJson> = match serde_json::from_str(&normalized) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    let mut map = HashMap::new();
    for d in disks {
        for letter in d.letters.split(',').filter(|s| !s.is_empty()) {
            let c = letter.chars().next().unwrap_or('\0');
            map.insert(
                c,
                DiskPhys {
                    bus: d.bus_type.clone(),
                    media: d.media_type.clone(),
                    friendly: d.friendly.clone(),
                    manufacturer: d.manufacturer.clone(),
                    spindle: d.spindle_speed.trim().parse::<u32>().ok(),
                },
            );
        }
    }
    map
}

/// Lê `GetLogicalDriveStringsW` → string "C:\\\0D:\\\0\0".
fn logical_drive_strings() -> Option<String> {
    let mut buf = [0u16; 512];
    let len = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) };
    if len == 0 || len as usize >= buf.len() {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn volume_info(mount: &str) -> (String, String) {
    let mut name = [0u16; 256];
    let mut fs = [0u16; 256];
    let mut serial = 0u32;
    let mut max_comp = 0u32;
    let mut flags = 0u32;
    let ok = unsafe {
        GetVolumeInformationW(
            enc_wide(mount).as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            &mut serial,
            &mut max_comp,
            &mut flags,
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return (String::new(), String::new());
    }
    (from_wide(&name), from_wide(&fs))
}

fn disk_space(mount: &str) -> (u64, u64) {
    let mut free_available: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    unsafe {
        GetDiskFreeSpaceExW(
            enc_wide(mount).as_ptr(),
            &mut free_available,
            &mut total,
            &mut total_free,
        )
    };
    (total, total_free)
}

fn enc_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}
