use crate::enumerate::{self, DirBuf, DirHandle};
use crate::node::{Node, DONE, LISTED, QUEUED, UNLISTED};
use crate::paths;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::*};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct Job {
    node: Arc<Node>,
    /// Open handle of the parent directory: the child is opened relative to it.
    parent: Option<Arc<DirHandle>>,
    /// Child name (no NUL) when `parent` is set, else the `\\?\`-prefixed, NUL terminated absolute path.
    name: Vec<u16>,
}

struct Shared {
    stack: Mutex<Vec<Job>>,
    cond: Condvar,
    /// Directories queued or being listed.
    pending: AtomicI64,
    version: AtomicU64,
    dirs_scanned: AtomicU64,
    items_scanned: AtomicU64,
    denied: AtomicU64,
    shutdown: AtomicBool,
}

/// Parallel directory-size scanner.
///
/// Workers pull directories from a LIFO stack (depth-first locality, and the folder the user just opened is
/// always on top), list them in bulk, add the files' allocated bytes to the directory and all of its ancestors
/// with atomic adds, and push sub-directories. The UI reads the atomics at any time for live sizes.
pub struct Scanner {
    shared: Arc<Shared>,
    pub root: Arc<Node>,
    threads: Vec<thread::JoinHandle<()>>,
}

enum Skip {
    None,
    Dataless,
}

struct Sub {
    off: usize,
    len: usize,
    skip: Skip,
}

impl Scanner {
    pub fn new(threads: usize) -> Scanner {
        let shared = Arc::new(Shared {
            stack: Mutex::new(Vec::with_capacity(1024)),
            cond: Condvar::new(),
            pending: AtomicI64::new(0),
            version: AtomicU64::new(0),
            dirs_scanned: AtomicU64::new(0),
            items_scanned: AtomicU64::new(0),
            denied: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
        });
        let root = Node::new(String::new(), None);
        let handles = (0..threads.max(1))
            .map(|i| {
                let s = shared.clone();
                thread::Builder::new()
                    .name(format!("SpaceMaster.scan.{i}"))
                    .stack_size(1 << 20)
                    .spawn(move || worker_loop(&s))
                    .expect("spawn scanner thread")
            })
            .collect();
        Scanner { shared, root, threads: handles }
    }

    /// One worker per logical processor.
    pub fn default_threads() -> usize {
        std::env::var("SM_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| thread::available_parallelism().map(|n| n.get()).unwrap_or(8).clamp(4, 8))
    }

    // MARK: Counters

    pub fn is_scanning(&self) -> bool {
        self.shared.pending.load(Relaxed) > 0
    }
    /// Incremented after each listed directory; the UI polls it to know whether anything changed.
    pub fn version(&self) -> u64 {
        self.shared.version.load(Relaxed)
    }
    pub fn dirs_scanned(&self) -> u64 {
        self.shared.dirs_scanned.load(Relaxed)
    }
    pub fn items_scanned(&self) -> u64 {
        self.shared.items_scanned.load(Relaxed)
    }
    pub fn denied(&self) -> u64 {
        self.shared.denied.load(Relaxed)
    }

    // MARK: Tree access

    /// Finds or creates the node for an absolute, normalized path ("" is the root); unscanned ancestors become
    /// placeholders.
    pub fn node_for_path(&self, path: &str) -> Arc<Node> {
        let mut node = self.root.clone();
        for comp in path.split('\\').filter(|c| !c.is_empty()) {
            let next = {
                let mut g = node.lock();
                match g.children.iter().find(|c| paths::eq_ci(&c.name, comp)) {
                    Some(c) => c.clone(),
                    None => {
                        let c = Node::new(comp.to_string(), Some(node.clone()));
                        g.children.push(c.clone());
                        c
                    }
                }
            };
            node = next;
        }
        node
    }

    /// Existing children of a node (snapshot).
    pub fn children(&self, node: &Node) -> Vec<Arc<Node>> {
        node.lock().children.clone()
    }

    pub fn is_unreadable(&self, node: &Node) -> bool {
        node.lock().unreadable
    }

    // MARK: Scanning

    /// Scans `node` (and its subtree) if it was never scanned. Runs before everything already queued.
    pub fn scan(&self, node: &Arc<Node>) {
        let accept = {
            let _g = node.lock();
            if node.raw_state() == UNLISTED {
                node.state.store(QUEUED, Release);
                true
            } else {
                false
            }
        };
        if !accept {
            return;
        }
        let path = paths::to_wide_nt(&node.path());
        self.push(vec![Job { node: node.clone(), parent: None, name: path }]);
    }

    /// Marks a node as an independent scan root (a drive): its bytes stay out of the ancestors.
    pub fn mark_mount_root(&self, node: &Arc<Node>) {
        let mut g = node.lock();
        node.mount_root.store(true, Relaxed);
        g.silent = true;
    }

    /// Throws away a subtree and scans it again.
    pub fn rescan(&self, node: &Arc<Node>) {
        let old = {
            let mut g = node.lock();
            let st = node.raw_state();
            if st != DONE && st != UNLISTED {
                return;
            }
            propagate(node, -node.total_bytes(), -node.item_count());
            g.remaining = 0;
            g.unreadable = false;
            g.silent = true;
            let old = std::mem::take(&mut g.children);
            node.state.store(UNLISTED, Release);
            old
        };
        // Freeing a big subtree can take a while: keep it off the caller's thread.
        if !old.is_empty() {
            thread::spawn(move || drop(old));
        }
        self.scan(node);
        self.shared.version.fetch_add(1, Relaxed);
    }

    /// Bookkeeping after files in `dir` were deleted (negative values after an undo).
    pub fn did_remove_files(&self, bytes: i64, items: i64, dir: &Arc<Node>) {
        propagate(dir, -bytes, -items);
        self.shared.version.fetch_add(1, Relaxed);
    }

    /// Bookkeeping after a whole sub-directory was deleted.
    pub fn did_remove_directory(&self, child: &Arc<Node>) {
        if let Some(parent) = &child.parent {
            parent.lock().children.retain(|c| !Arc::ptr_eq(c, child));
            if !child.is_mount_root() {
                propagate(parent, -child.total_bytes(), -child.item_count());
            }
        }
        self.shared.version.fetch_add(1, Relaxed);
    }

    /// Blocks until all queued work is finished (tests, benchmark).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while self.is_scanning() {
            if start.elapsed() > timeout {
                return false;
            }
            thread::sleep(Duration::from_micros(500));
        }
        true
    }

    fn push(&self, jobs: Vec<Job>) {
        push(&self.shared, jobs);
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, SeqCst);
        {
            let _g = self.shared.stack.lock();
        }
        self.shared.cond.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn push(shared: &Shared, jobs: Vec<Job>) {
    if jobs.is_empty() {
        return;
    }
    let n = jobs.len();
    shared.pending.fetch_add(n as i64, Relaxed);
    {
        let mut st = shared.stack.lock().unwrap_or_else(|e| e.into_inner());
        st.extend(jobs);
    }
    if n == 1 { shared.cond.notify_one() } else { shared.cond.notify_all() }
}

fn worker_loop(shared: &Shared) {
    // Scanning is background work: it must never take CPU from the foreground (video playback, scrolling).
    unsafe {
        let _ = windows::Win32::System::Threading::SetThreadPriority(
            windows::Win32::System::Threading::GetCurrentThread(),
            windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL,
        );
    }
    let mut buf = DirBuf::new();
    let mut names: Vec<u16> = Vec::with_capacity(4096);
    let mut subs: Vec<Sub> = Vec::with_capacity(256);
    loop {
        let job = {
            let mut st = shared.stack.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if shared.shutdown.load(SeqCst) {
                    return;
                }
                if let Some(j) = st.pop() {
                    break j;
                }
                st = shared.cond.wait(st).unwrap_or_else(|e| e.into_inner());
            }
        };
        process(shared, job, &mut buf, &mut names, &mut subs);
        shared.pending.fetch_sub(1, Relaxed);
    }
}

fn process(shared: &Shared, job: Job, buf: &mut DirBuf, names: &mut Vec<u16>, subs: &mut Vec<Sub>) {
    names.clear();
    subs.clear();
    let mut file_bytes: i64 = 0;
    let mut file_count: i64 = 0;
    let opened = match &job.parent {
        Some(p) => enumerate::open_relative(p, &job.name),
        None => enumerate::open_path(&job.name),
    };
    let mut handle: Option<Arc<DirHandle>> = None;
    let mut failed = true;
    if let Ok(h) = opened {
        let result = enumerate::list_handle(&h, buf, |e| {
            if e.is_dir() && !e.is_link() {
                let off = names.len();
                names.extend_from_slice(e.name);
                subs.push(Sub { off, len: e.name.len(), skip: if e.is_dataless_dir() { Skip::Dataless } else { Skip::None } });
            } else {
                file_bytes = file_bytes.wrapping_add(e.alloc);
                file_count += 1;
            }
        });
        failed = result.is_err();
        // Children are opened relative to this handle; without sub-directories it can go right away.
        if subs.iter().any(|s| matches!(s.skip, Skip::None)) {
            handle = Some(Arc::new(h));
        }
    }
    if failed {
        shared.denied.fetch_add(1, Relaxed);
    }

    let node = &job.node;
    let mut spawn: Vec<Job> = Vec::new();
    {
        let mut g = node.lock();
        let mut remaining = 1; // our own token, released after spawning
        let mut existing: std::collections::HashMap<String, Arc<Node>> = std::collections::HashMap::new();
        if !g.children.is_empty() {
            existing.reserve(g.children.len());
            for c in &g.children {
                existing.insert(c.name.clone(), c.clone());
            }
        }
        g.children.reserve(subs.len());
        for s in subs.iter() {
            let wide = &names[s.off..s.off + s.len];
            let name = String::from_utf16_lossy(wide);
            let child = match existing.get(&name) {
                Some(c) => c.clone(),
                None => {
                    let c = Node::new(name, Some(node.clone()));
                    g.children.push(c.clone());
                    c
                }
            };
            match s.skip {
                Skip::Dataless => child.state.store(DONE, Relaxed),
                Skip::None => match child.raw_state() {
                    UNLISTED => {
                        child.state.store(QUEUED, Relaxed);
                        spawn.push(Job { node: child, parent: handle.clone(), name: wide.to_vec() });
                        remaining += 1;
                    }
                    QUEUED | LISTED => remaining += 1,
                    _ => {}
                },
            }
        }
        g.unreadable = failed;
        g.remaining = remaining;
        node.state.store(LISTED, Release);
    }

    let items = file_count + subs.len() as i64;
    if file_bytes != 0 || items != 0 {
        // Lock-free: atomic adds up the ancestor chain (stops at drive roots).
        propagate(node, file_bytes, items);
    }
    shared.dirs_scanned.fetch_add(1, Relaxed);
    shared.items_scanned.fetch_add(items as u64, Relaxed);
    shared.version.fetch_add(1, Relaxed);

    push(shared, spawn);
    drop(handle);

    let finished = {
        let mut g = node.lock();
        g.remaining -= 1;
        g.remaining == 0
    };
    if finished {
        complete(node);
    }
}

/// Adds to the node and every ancestor, stopping after a drive root. Atomics only.
fn propagate(node: &Arc<Node>, bytes: i64, items: i64) {
    let mut n: &Node = node;
    loop {
        if bytes != 0 {
            n.total.fetch_add(bytes, Relaxed);
        }
        if items != 0 {
            n.items.fetch_add(items, Relaxed);
        }
        if n.is_mount_root() {
            break;
        }
        match &n.parent {
            Some(p) => n = p,
            None => break,
        }
    }
}

/// Marks `start` done and cascades up while parents have no outstanding children.
fn complete(start: &Arc<Node>) {
    let mut n = start.clone();
    loop {
        n.state.store(DONE, Release);
        let was_silent = {
            let mut g = n.lock();
            let w = g.silent;
            g.silent = n.is_mount_root();
            w
        };
        if was_silent {
            return;
        }
        let Some(p) = n.parent.clone() else { return };
        let next = {
            let mut g = p.lock();
            if p.raw_state() != LISTED {
                return;
            }
            g.remaining -= 1;
            g.remaining == 0
        };
        if next {
            n = p;
        } else {
            return;
        }
    }
}
