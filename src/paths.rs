//! Windows path helpers. Paths in SpaceMaster are absolute, backslash separated, without a `\\?\` prefix
//! (`C:\Users\me`). The empty string is the virtual "This PC" root that lists the drives.

/// Joins a directory and an entry name. `join("", "C:")` is the drive root `C:\`.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        format!("{name}\\")
    } else if dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

/// Parent directory; the parent of a drive root is "" (This PC) and "" has no parent.
pub fn parent(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let t = path.trim_end_matches('\\');
    match t.rfind('\\') {
        Some(i) => {
            let p = &t[..i];
            if p.len() == 2 && p.ends_with(':') { Some(format!("{p}\\")) } else { Some(p.to_string()) }
        }
        None => Some(String::new()),
    }
}

/// Last component; `C:\` gives `C:`.
pub fn file_name(path: &str) -> &str {
    let t = path.trim_end_matches('\\');
    match t.rfind('\\') {
        Some(i) => &t[i + 1..],
        None => t,
    }
}

pub fn is_drive_root(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() == 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
}

pub fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || (!a.is_ascii() || !b.is_ascii()) && a.to_lowercase() == b.to_lowercase()
}

/// Case-insensitive "is `path` equal to or inside `base`" (on component boundaries).
pub fn is_within(path: &str, base: &str) -> bool {
    let base = base.trim_end_matches('\\');
    if path.len() < base.len() || !path.is_char_boundary(base.len()) {
        return false;
    }
    if !eq_ci(&path[..base.len()], base) {
        return false;
    }
    path.len() == base.len() || path.as_bytes()[base.len()] == b'\\'
}

/// Expands `~` and `%VAR%`, converts `/` to `\`, resolves `.` and `..`, strips `\\?\`.
/// Returns `None` for anything that is not an absolute drive path (UNC paths are not supported).
pub fn normalize(raw: &str, home: &str) -> Option<String> {
    let mut s = raw.trim().trim_matches('"').trim().to_string();
    if s.is_empty() {
        return None;
    }
    if s == "~" || s.starts_with("~\\") || s.starts_with("~/") {
        s = format!("{home}{}", &s[1..]);
    }
    if s.contains('%') {
        s = expand_env(&s);
    }
    s = s.replace('/', "\\");
    if let Some(r) = s.strip_prefix("\\\\?\\") {
        s = r.to_string();
    }
    let b = s.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None;
    }
    let drive = s[..1].to_ascii_uppercase();
    let mut comps: Vec<&str> = Vec::new();
    for c in s[2..].split('\\') {
        match c {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            c => comps.push(c),
        }
    }
    let mut out = format!("{drive}:");
    if comps.is_empty() {
        out.push('\\');
    } else {
        for c in comps {
            out.push('\\');
            out.push_str(c);
        }
    }
    Some(out)
}

fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) => {
                let name = &after[..j];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// UTF-16, `\\?\`-prefixed (long path capable, skips Win32 path normalisation), NUL terminated.
pub fn to_wide_nt(path: &str) -> Vec<u16> {
    let mut v: Vec<u16> = Vec::with_capacity(path.len() + 6);
    v.extend("\\\\?\\".encode_utf16());
    v.extend(path.encode_utf16());
    v.push(0);
    v
}

/// UTF-16, NUL terminated, no prefix (for shell APIs that do not understand `\\?\`).
pub fn to_wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// Home directory (`%USERPROFILE%`).
pub fn home_dir() -> String {
    std::env::var("USERPROFILE").ok().and_then(|p| normalize(&p, "C:\\")).unwrap_or_else(|| "C:\\".to_string())
}

/// Natural, case-insensitive ordering like Explorer: `file2` < `file10`.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ia = a.chars().peekable();
    let mut ib = b.chars().peekable();
    loop {
        match (ia.peek().copied(), ib.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let mut na = String::new();
                    while let Some(&c) = ia.peek() {
                        if c.is_ascii_digit() { na.push(c); ia.next(); } else { break; }
                    }
                    let mut nb = String::new();
                    while let Some(&c) = ib.peek() {
                        if c.is_ascii_digit() { nb.push(c); ib.next(); } else { break; }
                    }
                    let ta = na.trim_start_matches('0');
                    let tb = nb.trim_start_matches('0');
                    let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                    if o != Ordering::Equal {
                        return o;
                    }
                } else {
                    ia.next();
                    ib.next();
                    let la = ca.to_lowercase().next().unwrap_or(ca);
                    let lb = cb.to_lowercase().next().unwrap_or(cb);
                    let o = la.cmp(&lb);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
            }
        }
    }
}

