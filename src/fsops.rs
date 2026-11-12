//! Deleting: Recycle Bin (with restore), and fast permanent deletion.

use crate::paths;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::{SHFileOperationW, FO_DELETE, SHFILEOPSTRUCTW};

const FOF_SILENT: u16 = 0x0004;
const FOF_NOCONFIRMMKDIR: u16 = 0x0200;
const FOF_NOERRORUI: u16 = 0x0400;
const FOF_ALLOWUNDO: u16 = 0x0040;
const FOF_WANTNUKEWARNING: u16 = 0x4000;

/// Moves one item to the Recycle Bin. Items the bin cannot take (too big, no bin on that drive) trigger
/// Windows' own "delete permanently?" question instead of vanishing silently.
pub fn trash(path: &str, owner: isize) -> Result<(), String> {
    let mut from: Vec<u16> = path.encode_utf16().collect();
    from.push(0);
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: windows::Win32::Foundation::HWND(owner as *mut _),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        pTo: PCWSTR::null(),
        fFlags: FOF_ALLOWUNDO | FOF_SILENT | FOF_NOCONFIRMMKDIR | FOF_NOERRORUI | FOF_WANTNUKEWARNING,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if op.fAnyOperationsAborted.as_bool() {
        return Err("Cancelled".into());
    }
    if r != 0 && Path::new(path).symlink_metadata().is_ok() {
        return Err(format!("Windows could not move it to the Recycle Bin (error 0x{r:X})."));
    }
    if Path::new(path).symlink_metadata().is_ok() {
        return Err("Windows could not move it to the Recycle Bin.".into());
    }
    Ok(())
}

/// Deletes a file, symlink or whole directory tree for good. Large trees are removed with several threads.
pub fn delete_permanent(path: &str) -> io::Result<()> {
    let p = Path::new(path);
    let meta = p.symlink_metadata()?;
    if meta.file_type().is_symlink() {
        // A link is removed, never its target. Directory links need remove_dir.
        return remove_file_force(p).or_else(|_| std::fs::remove_dir(p));
    }
    if meta.is_dir() {
        par_remove_dir(p, 2)?;
        Ok(())
    } else {
        remove_file_force(p)
    }
}

fn remove_file_force(p: &Path) -> io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            if let Ok(m) = p.metadata() {
                let mut perm = m.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                let _ = std::fs::set_permissions(p, perm);
            }
            std::fs::remove_file(p)
        }
        r => r,
    }
}

fn par_remove_dir(dir: &Path, depth: u32) -> io::Result<()> {
    if depth == 0 {
        return std::fs::remove_dir_all(dir);
    }
    let mut children: Vec<(std::path::PathBuf, bool)> = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let ft = e.file_type()?;
        children.push((e.path(), ft.is_dir() && !ft.is_symlink()));
    }
    if children.len() < 4 {
        return std::fs::remove_dir_all(dir);
    }
    let queue = Mutex::new(children);
    let first_err: Mutex<Option<io::Error>> = Mutex::new(None);
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(12);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let next = queue.lock().unwrap().pop();
                let Some((p, is_dir)) = next else { break };
                let r = if is_dir { par_remove_dir(&p, depth - 1) } else { remove_file_force(&p) };
                if let Err(e) = r {
                    let mut g = first_err.lock().unwrap();
                    if g.is_none() {
                        *g = Some(e);
                    }
                }
            });
        }
    });
    if let Some(e) = first_err.into_inner().unwrap() {
        return Err(e);
    }
    std::fs::remove_dir(dir)
}

/// Puts an item that was moved to the Recycle Bin back where it came from. Reads the bin's `$I` metadata files
/// (original path) and renames the matching `$R` data item. The most recently deleted match wins.
pub fn restore_from_recycle_bin(original: &str) -> Result<(), String> {
    let drive = original.get(..2).ok_or("invalid path")?;
    let bin = format!("{drive}\\$Recycle.Bin");
    let mut best: Option<(i64, std::path::PathBuf, std::path::PathBuf)> = None;
    let sids = std::fs::read_dir(&bin).map_err(|e| format!("Can't read the Recycle Bin: {e}"))?;
    for sid in sids.flatten() {
        let Ok(files) = std::fs::read_dir(sid.path()) else { continue };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if !name.starts_with("$I") {
                continue;
            }
            let Ok(bytes) = std::fs::read(f.path()) else { continue };
            let Some((when, orig)) = parse_i_file(&bytes) else { continue };
            if !paths::eq_ci(&orig, original) {
                continue;
            }
            let data = sid.path().join(format!("$R{}", &name[2..]));
            if data.symlink_metadata().is_err() {
                continue;
            }
            if best.as_ref().is_none_or(|(t, _, _)| when >= *t) {
                best = Some((when, f.path(), data));
            }
        }
    }
    let (_, meta, data) = best.ok_or("It is no longer in the Recycle Bin.")?;
    if Path::new(original).symlink_metadata().is_ok() {
        return Err("Something with that name already exists.".into());
    }
    std::fs::rename(&data, original).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(meta);
    Ok(())
}

/// Parses a Recycle Bin `$I` file: (deletion FILETIME, original path).
pub fn parse_i_file(b: &[u8]) -> Option<(i64, String)> {
    if b.len() < 28 {
        return None;
    }
    let version = i64::from_le_bytes(b[0..8].try_into().ok()?);
    let when = i64::from_le_bytes(b[16..24].try_into().ok()?);
    let (start, chars) = match version {
        1 => (24usize, (b.len().saturating_sub(24)) / 2),
        2 => {
            let n = u32::from_le_bytes(b[24..28].try_into().ok()?) as usize;
            (28usize, n.min((b.len() - 28) / 2))
        }
        _ => return None,
    };
    let units: Vec<u16> = b[start..start + chars * 2]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    Some((when, String::from_utf16_lossy(&units)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sm-fsops-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parse_v2_i_file() {
        let path = "C:\\temp\\a.txt";
        let mut b = Vec::new();
        b.extend(2i64.to_le_bytes());
        b.extend(123i64.to_le_bytes());
        b.extend(777i64.to_le_bytes());
        let w: Vec<u16> = path.encode_utf16().chain([0]).collect();
        b.extend((w.len() as u32).to_le_bytes());
        for c in w {
            b.extend(c.to_le_bytes());
        }
        assert_eq!(parse_i_file(&b), Some((777, path.to_string())));
        assert_eq!(parse_i_file(&b[..10]), None);
    }

    #[test]
    fn permanent_delete_tree() {
        let d = tmp("tree");
        for i in 0..30 {
            let sub = d.join(format!("d{i}")).join("x").join("y");
            std::fs::create_dir_all(&sub).unwrap();
            for j in 0..5 {
                std::fs::write(sub.join(format!("f{j}.bin")), b"hello").unwrap();
            }
        }
        // A read-only file must not block deletion.
        let ro = d.join("readonly.txt");
        std::fs::write(&ro, b"x").unwrap();
        let mut perm = ro.metadata().unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&ro, perm).unwrap();
        delete_permanent(d.to_str().unwrap()).unwrap();
        assert!(!d.exists());
    }

    #[test]
    fn permanent_delete_single_file_and_missing() {
        let d = tmp("single");
        let f = d.join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        delete_permanent(f.to_str().unwrap()).unwrap();
        assert!(!f.exists());
        assert!(delete_permanent(f.to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn recycle_bin_roundtrip() {
        let d = tmp("bin");
        let f = d.join("restore-me.txt");
        std::fs::write(&f, b"precious").unwrap();
        let p = f.to_str().unwrap().to_string();
        trash(&p, 0).expect("moved to the recycle bin");
        assert!(!f.exists());
        restore_from_recycle_bin(&p).expect("restored");
        assert_eq!(std::fs::read(&f).unwrap(), b"precious");
        let _ = std::fs::remove_dir_all(d);
    }
}
