use crate::paths;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows::Win32::System::Diagnostics::Debug::{SetErrorMode, SEM_FAILCRITICALERRORS, THREAD_ERROR_MODE};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    pub letter: char,
    /// `C:\`
    pub path: String,
    /// Volume label, may be empty.
    pub label: String,
    pub total: i64,
    pub available: i64,
    pub is_internal: bool,
    pub is_system: bool,
}

impl VolumeInfo {
    pub fn used(&self) -> i64 {
        (self.total - self.available).max(0)
    }
    pub fn drive(&self) -> String {
        format!("{}:", self.letter)
    }
    /// "Windows (C:)" or "Local Disk (C:)".
    pub fn display_name(&self) -> String {
        let l = if self.label.is_empty() { "Local Disk" } else { &self.label };
        format!("{l} ({}:)", self.letter)
    }
    pub fn fraction_used(&self) -> f64 {
        if self.total <= 0 { 0.0 } else { self.used() as f64 / self.total as f64 }
    }
}

/// Fixed and removable drives that are ready (no CD-ROMs, no network shares, no empty card readers).
pub fn list() -> Vec<VolumeInfo> {
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_ascii_uppercase();
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let path = format!("{letter}:\\");
        let w = paths::to_wide(&path);
        let ty = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) };
        // DRIVE_REMOVABLE = 2, DRIVE_FIXED = 3
        if ty != 2 && ty != 3 {
            continue;
        }
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        let prev = unsafe { SetErrorMode(SEM_FAILCRITICALERRORS) };
        let ok = unsafe { GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut avail), Some(&mut total), Some(&mut free)) };
        let mut label_buf = [0u16; 261];
        let _ = unsafe {
            GetVolumeInformationW(PCWSTR(w.as_ptr()), Some(&mut label_buf), None, None, None, None)
        };
        unsafe {
            SetErrorMode(THREAD_ERROR_MODE(prev.0));
        }
        if ok.is_err() || total == 0 {
            continue;
        }
        let n = label_buf.iter().position(|&c| c == 0).unwrap_or(0);
        out.push(VolumeInfo {
            letter,
            path,
            label: String::from_utf16_lossy(&label_buf[..n]),
            total: total as i64,
            available: avail as i64,
            is_internal: ty == 3,
            is_system: format!("{letter}:") == system,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_system_drive() {
        let v = list();
        assert!(!v.is_empty());
        let sys = v.iter().find(|v| v.is_system).expect("system drive listed");
        assert!(sys.total > 0 && sys.available >= 0 && sys.available <= sys.total);
        assert_eq!(sys.path.len(), 3);
        assert!(sys.display_name().ends_with(":)"));
        assert!((0.0..=1.0).contains(&sys.fraction_used()));
    }
}
