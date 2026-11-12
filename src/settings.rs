//! Tiny key=value settings file in `%APPDATA%\SpaceMaster\settings.ini` (no serialization dependency).

use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct Settings {
    path: Option<PathBuf>,
    map: BTreeMap<String, String>,
}

impl Settings {
    /// Settings stored at the default location.
    pub fn load_default() -> Settings {
        let path = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("SpaceMaster").join("settings.ini"));
        Self::load(path)
    }

    pub fn load(path: Option<PathBuf>) -> Settings {
        let mut map = BTreeMap::new();
        if let Some(p) = &path {
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    if let Some((k, v)) = line.split_once('=') {
                        map.insert(k.trim().to_string(), v.trim().to_string());
                    }
                }
            }
        }
        Settings { path, map }
    }

    /// In-memory only (tests).
    pub fn memory() -> Settings {
        Settings { path: None, map: BTreeMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|s| s.as_str())
    }
    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            Some("1") | Some("true") => true,
            Some("0") | Some("false") => false,
            _ => default,
        }
    }
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.parse().ok())
    }
    pub fn set(&mut self, key: &str, value: impl ToString) {
        self.map.insert(key.to_string(), value.to_string().replace(['\r', '\n'], " "));
    }

    pub fn save(&self) {
        let Some(p) = &self.path else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text: String = self.map.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        let _ = std::fs::write(p, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("sm-settings-{}", std::process::id()));
        let file = dir.join("s.ini");
        let mut s = Settings::load(Some(file.clone()));
        s.set("sort", "size");
        s.set("hidden", true);
        s.set("lastPath", "C:\\Users\\me");
        s.save();
        let t = Settings::load(Some(file));
        assert_eq!(t.get("sort"), Some("size"));
        assert!(t.get_bool("hidden", false));
        assert!(t.get_bool("missing", true));
        assert_eq!(t.get("lastPath"), Some("C:\\Users\\me"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
