use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub const UNLISTED: u8 = 0; // never scanned
pub const QUEUED: u8 = 1; // waiting for / being listed by a worker
pub const LISTED: u8 = 2; // this directory listed, descendants still running
pub const DONE: u8 = 3; // whole subtree scanned

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Unlisted,
    Queued,
    Listed,
    Done,
}

pub(crate) struct Inner {
    pub children: Vec<Arc<Node>>,
    pub remaining: i32,
    /// Does not notify its parent on completion (drives, nodes being rescanned).
    pub silent: bool,
    /// Directory could not be read (permissions).
    pub unreadable: bool,
}

/// A directory in the scan tree. Files are not nodes: their bytes are summed into the directory's totals
/// (and re-listed on demand when the UI shows the folder), so memory scales with the number of directories.
/// The virtual root (empty name, no parent) is "This PC"; its children are the drives.
pub struct Node {
    pub name: String,
    pub parent: Option<Arc<Node>>,
    pub(crate) total: AtomicI64,
    pub(crate) items: AtomicI64,
    pub(crate) state: AtomicU8,
    /// Drives: own scan root, their bytes are not added to ancestors.
    pub(crate) mount_root: AtomicBool,
    pub(crate) inner: Mutex<Inner>,
}

impl Node {
    pub(crate) fn new(name: String, parent: Option<Arc<Node>>) -> Arc<Node> {
        Arc::new(Node {
            name,
            parent,
            total: AtomicI64::new(0),
            items: AtomicI64::new(0),
            state: AtomicU8::new(UNLISTED),
            mount_root: AtomicBool::new(false),
            inner: Mutex::new(Inner { children: Vec::new(), remaining: 0, silent: false, unreadable: false }),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Bytes of everything scanned so far in this subtree. Live while scanning, final once done.
    #[inline]
    pub fn total_bytes(&self) -> i64 {
        self.total.load(Ordering::Relaxed)
    }
    /// Number of files + directories scanned so far in this subtree.
    #[inline]
    pub fn item_count(&self) -> i64 {
        self.items.load(Ordering::Relaxed)
    }
    #[inline]
    pub fn raw_state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }
    pub fn state(&self) -> State {
        match self.raw_state() {
            UNLISTED => State::Unlisted,
            QUEUED => State::Queued,
            LISTED => State::Listed,
            _ => State::Done,
        }
    }
    #[inline]
    pub fn is_done(&self) -> bool {
        self.raw_state() == DONE
    }
    #[inline]
    pub fn is_mount_root(&self) -> bool {
        self.mount_root.load(Ordering::Relaxed)
    }

    /// Absolute path: "" for the root, `C:\` for a drive, `C:\Users\me` below.
    pub fn path(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let mut n = self;
        while let Some(p) = &n.parent {
            parts.push(&n.name);
            n = p;
        }
        parts.reverse();
        match parts.len() {
            0 => String::new(),
            1 => format!("{}\\", parts[0]),
            _ => parts.join("\\"),
        }
    }
}
