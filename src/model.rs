//! Shared application model: scanner, safety rules, drives.

use crate::listing::{self, Entry, Kind};
use crate::node::{Node, State};
use crate::paths;
use crate::safety::{Context, Safety, SafetyClassifier};
use crate::scanner::Scanner;
use crate::volumes::{self, VolumeInfo};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct Model {
    pub scanner: Scanner,
    pub classifier: SafetyClassifier,
    pub home: String,
    /// Scan the whole drive of the start folder in the background (the app's default; tests turn it off).
    pub scan_drive: std::sync::atomic::AtomicBool,
    volumes: Mutex<Vec<VolumeInfo>>,
}

impl Model {
    pub fn new() -> Arc<Model> {
        Self::with(Scanner::default_threads(), SafetyClassifier::new())
    }

    pub fn with(threads: usize, classifier: SafetyClassifier) -> Arc<Model> {
        let home = classifier.home().to_string();
        Arc::new(Model {
            scanner: Scanner::new(threads),
            classifier,
            home,
            scan_drive: std::sync::atomic::AtomicBool::new(true),
            volumes: Mutex::new(volumes::list()),
        })
    }

    pub fn volumes(&self) -> Vec<VolumeInfo> {
        self.volumes.lock().unwrap().clone()
    }

    pub fn refresh_volumes(&self) {
        let v = volumes::list();
        *self.volumes.lock().unwrap() = v;
    }

    pub fn volume_containing(&self, path: &str) -> Option<VolumeInfo> {
        let v = self.volumes.lock().unwrap();
        v.iter().find(|v| path.len() >= 2 && paths::eq_ci(&path[..2], &v.drive())).cloned()
    }

    /// Node for a path; drives become independent scan roots.
    pub fn node(&self, path: &str) -> Arc<Node> {
        let n = self.scanner.node_for_path(path);
        let mut d = n.clone();
        while let Some(p) = d.parent.clone() {
            if p.parent.is_none() {
                if !d.is_mount_root() {
                    self.scanner.mark_mount_root(&d);
                }
                break;
            }
            d = p;
        }
        n
    }

    /// Queues a scan (never for the virtual "This PC" root).
    pub fn scan(&self, path: &str) {
        if !path.is_empty() {
            self.scanner.scan(&self.node(path));
        }
    }

    /// Starts the drive that holds `path` in the background, with `path` itself on top.
    pub fn start_scanning(&self, path: &str) {
        if self.scan_drive.load(std::sync::atomic::Ordering::Relaxed) && path.len() >= 3 {
            self.scan(&path[..3]);
        }
        self.scan(path);
    }

    /// Existing directory for user input: expands `~` and `%VARS%`, fixes the case, follows nothing.
    pub fn resolve(&self, raw: &str) -> Option<String> {
        let t = raw.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("this pc") || t == "\\" {
            return Some(String::new());
        }
        let p = paths::normalize(t, &self.home)?;
        let meta = std::fs::metadata(&p).ok()?;
        if !meta.is_dir() {
            return None;
        }
        // Canonical spelling (case), so typed paths map onto the scanner's nodes.
        match std::fs::canonicalize(&p) {
            Ok(c) => paths::normalize(&c.to_string_lossy(), &self.home).or(Some(p)),
            Err(_) => Some(p),
        }
    }
}

// MARK: Rows

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Size,
    Name,
    Modified,
}

impl SortKey {
    pub fn title(self) -> &'static str {
        match self {
            SortKey::Size => "Size",
            SortKey::Name => "Name",
            SortKey::Modified => "Date Modified",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            SortKey::Size => "size",
            SortKey::Name => "name",
            SortKey::Modified => "modified",
        }
    }
    pub fn from_id(s: &str) -> Option<SortKey> {
        match s {
            "size" => Some(SortKey::Size),
            "name" => Some(SortKey::Name),
            "modified" => Some(SortKey::Modified),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Row {
    pub name: String,
    pub kind: Kind,
    pub size: i64,
    pub mtime: i64,
    pub hidden: bool,
    pub safety: Safety,
    pub attrs: u32,
    pub node: Option<Arc<Node>>,
    /// Subtree still being scanned (size is a lower bound).
    pub calculating: bool,
    /// Directory that was never scanned: size unknown.
    pub unknown_size: bool,
}

impl Row {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Directory
    }
}

pub type StateRef = Rc<RefCell<DirState>>;

/// One directory as displayed in a column: raw listing + sorted/filtered rows.
pub struct DirState {
    pub path: String,
    pub node: Arc<Node>,
    pub entries: Vec<Entry>,
    pub error: Option<String>,
    pub ctx: Context,
    pub rows: Vec<Row>,
    /// Sum of every entry (hidden or filtered included): the denominator for percentages.
    pub total: i64,
    pub max_size: i64,
    pub any_calculating: bool,
    pub loaded_at: Instant,
}

impl DirState {
    pub fn load(model: &Model, path: &str) -> DirState {
        let node = model.node(path);
        let ctx = model.classifier.context(path);
        let (entries, error) = match listing::list(path) {
            Ok(e) => (e, None),
            Err(e) => (Vec::new(), Some(e.message)),
        };
        DirState {
            path: path.to_string(),
            node,
            entries,
            error,
            ctx,
            rows: Vec::new(),
            total: 0,
            max_size: 1,
            any_calculating: false,
            loaded_at: Instant::now(),
        }
    }

    pub fn rebuild(&mut self, model: &Model, sort: SortKey, reversed: bool, show_hidden: bool, filter: &str) {
        let this_pc = self.path.is_empty();
        let vols = if this_pc { model.volumes() } else { Vec::new() };
        let mut by_name: std::collections::HashMap<&str, Arc<Node>> = std::collections::HashMap::new();
        let kids;
        if self.entries.iter().any(|e| e.is_dir()) {
            kids = model.scanner.children(&self.node);
            by_name.reserve(kids.len());
            for k in &kids {
                by_name.insert(k.name.as_str(), k.clone());
            }
        }
        let needle = filter.to_lowercase();
        let mut rows: Vec<Row> = Vec::with_capacity(self.entries.len());
        let mut total = 0i64;
        let mut calculating = false;
        for e in &self.entries {
            let mut size = e.size;
            let (mut calc, mut unknown) = (false, false);
            let mut node = None;
            if e.is_dir() {
                if this_pc {
                    // Drives: show the used space right away; the scan refines nothing, it is exact already.
                    size = vols.iter().find(|v| v.drive().eq_ignore_ascii_case(&e.name)).map(|v| v.used()).unwrap_or(0);
                    node = by_name.get(e.name.as_str()).cloned();
                } else if let Some(n) = by_name.get(e.name.as_str()) {
                    size = n.total_bytes();
                    match n.state() {
                        State::Done => {}
                        State::Unlisted => {
                            unknown = true;
                            size = 0;
                        }
                        _ => calc = true,
                    }
                    node = Some(n.clone());
                } else {
                    unknown = true;
                    size = 0;
                }
            }
            total += size;
            calculating |= calc;
            if !show_hidden && e.hidden {
                continue;
            }
            if !needle.is_empty() && !e.name.to_lowercase().contains(&needle) {
                continue;
            }
            rows.push(Row {
                name: e.name.clone(),
                kind: e.kind,
                size,
                mtime: e.mtime,
                hidden: e.hidden,
                safety: model.classifier.classify(&e.name, e.attrs, e.is_dir(), &self.ctx),
                attrs: e.attrs,
                node,
                calculating: calc,
                unknown_size: unknown,
            });
        }
        match sort {
            SortKey::Size => rows.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| paths::natural_cmp(&a.name, &b.name))),
            SortKey::Name => rows.sort_by(|a, b| paths::natural_cmp(&a.name, &b.name)),
            SortKey::Modified => rows.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| paths::natural_cmp(&a.name, &b.name))),
        }
        // Size and date default to descending; name to ascending. `reversed` flips whichever applies.
        if reversed {
            rows.reverse();
        }
        self.max_size = rows.iter().map(|r| r.size).max().unwrap_or(1).max(1);
        self.rows = rows;
        self.total = total;
        self.any_calculating = calculating;
    }

    pub fn index_of(&self, name: Option<&str>) -> Option<usize> {
        let name = name?;
        self.rows.iter().position(|r| r.name == name)
    }
}

/// Heat colour by share of the folder: large = red, mid = green, tiny = blue. Returns (hue 0..1 where
/// 0 = red and 0.64 = blue), bucketed so the UI can cache brushes.
pub fn share_bucket(share: f64) -> usize {
    let s = share.max(1e-4);
    let t = ((s.log10() + 3.0) / 3.0).clamp(0.0, 1.0);
    (t * 48.0) as usize
}

pub fn share_hue(bucket: usize) -> f32 {
    let tt = bucket as f32 / 48.0;
    (1.0 - tt) * 0.64
}
