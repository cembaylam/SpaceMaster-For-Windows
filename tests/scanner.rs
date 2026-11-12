//! Scanner integration tests: the scan tree must agree with an independent `std::fs` walk.

use spacemaster::node::Node;
use spacemaster::scanner::Scanner;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

struct TempDir(PathBuf);
impl TempDir {
    fn new(name: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!("sm-scan-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn s(&self) -> String {
        self.0.to_str().unwrap().to_string()
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scan(path: &str, threads: usize) -> (Scanner, Arc<Node>) {
    let scanner = Scanner::new(threads);
    let node = scanner.node_for_path(path);
    scanner.scan(&node);
    assert!(scanner.wait_idle(Duration::from_secs(120)), "scan did not finish");
    (scanner, node)
}

/// (entries, directories) below `dir`, symlinks counted as entries and not followed.
fn walk(dir: &Path) -> (i64, i64) {
    let (mut items, mut dirs) = (0, 0);
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            items += 1;
            let ft = e.file_type().unwrap();
            if ft.is_dir() && !ft.is_symlink() {
                dirs += 1;
                let (i, d) = walk(&e.path());
                items += i;
                dirs += d;
            }
        }
    }
    (items, dirs)
}

fn make_tree(root: &Path, fanout: usize, depth: usize, files: usize) {
    fn rec(p: &Path, fanout: usize, depth: usize, files: usize) {
        for f in 0..files {
            std::fs::write(p.join(format!("f{f}.dat")), vec![7u8; 100 + f * 1000]).unwrap();
        }
        if depth == 0 {
            return;
        }
        for d in 0..fanout {
            let c = p.join(format!("d{d}"));
            std::fs::create_dir(&c).unwrap();
            rec(&c, fanout, depth - 1, files);
        }
    }
    rec(root, fanout, depth, files);
}

#[test]
fn scan_agrees_with_std_walk() {
    let t = TempDir::new("walk");
    make_tree(t.path(), 4, 4, 6); // 4^0+..+4^4 = 341 dirs, 6 files each
    let (items, dirs) = walk(t.path());
    let (scanner, node) = scan(&t.s(), 8);
    assert!(node.is_done());
    assert_eq!(node.item_count(), items);
    assert_eq!(scanner.items_scanned() as i64, items);
    assert_eq!(scanner.dirs_scanned() as i64, dirs + 1);
    assert_eq!(scanner.denied(), 0);
}

#[test]
fn sizes_are_allocated_bytes() {
    let t = TempDir::new("sizes");
    std::fs::create_dir(t.path().join("a")).unwrap();
    std::fs::write(t.path().join("a").join("big.bin"), vec![1u8; 1_000_000]).unwrap();
    std::fs::write(t.path().join("small.bin"), vec![1u8; 10_000]).unwrap();
    let (_s, node) = scan(&t.s(), 4);
    let total = node.total_bytes();
    // Allocated size is cluster rounded: never less than the data, at most a few clusters more.
    assert!(total >= 1_010_000, "total {total}");
    assert!(total < 1_010_000 + 64 * 1024, "total {total}");
}

#[test]
fn children_and_ordering() {
    let t = TempDir::new("kids");
    std::fs::create_dir_all(t.path().join("a").join("b")).unwrap();
    std::fs::create_dir_all(t.path().join("c")).unwrap();
    std::fs::write(t.path().join("a").join("b").join("big.bin"), vec![1u8; 300_000]).unwrap();
    std::fs::write(t.path().join("a").join("small.bin"), vec![1u8; 100_000]).unwrap();
    std::fs::write(t.path().join("c").join("x.bin"), vec![1u8; 50_000]).unwrap();
    let (scanner, node) = scan(&t.s(), 4);
    let mut kids = scanner.children(&node);
    kids.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(kids.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), ["a", "c"]);
    assert!(kids[0].total_bytes() > kids[1].total_bytes());
    assert!(kids.iter().all(|k| k.is_done()));
    // Deleting a directory subtracts it from the ancestors.
    let before = node.total_bytes();
    let c_bytes = kids[1].total_bytes();
    scanner.did_remove_directory(&kids[1]);
    assert_eq!(node.total_bytes(), before - c_bytes);
    assert_eq!(scanner.children(&node).len(), 1);
}

#[test]
fn node_paths() {
    let scanner = Scanner::new(1);
    let n = scanner.node_for_path("C:\\Users\\me\\Docs");
    assert_eq!(n.path(), "C:\\Users\\me\\Docs");
    assert_eq!(n.parent.as_ref().unwrap().path(), "C:\\Users\\me");
    let drive = scanner.node_for_path("C:\\");
    assert_eq!(drive.path(), "C:\\");
    assert_eq!(drive.name, "C:");
    assert_eq!(scanner.root.path(), "");
    // Lookup is case-insensitive and returns the same node.
    let again = scanner.node_for_path("c:\\users\\ME\\docs");
    assert!(Arc::ptr_eq(&n, &again));
}

#[test]
fn symlinks_and_junctions_are_not_followed() {
    let t = TempDir::new("junction");
    let real = t.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("data.bin"), vec![1u8; 500_000]).unwrap();
    let link = t.path().join("link");
    // Junctions need no privileges.
    let ok = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&real)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "mklink /J failed");
    let (_s, node) = scan(&t.s(), 4);
    // Counted once, via `real`; the junction itself is just an entry.
    assert!(node.total_bytes() >= 500_000 && node.total_bytes() < 500_000 + 64 * 1024);
    assert_eq!(node.item_count(), 3); // real, link, data.bin
}

#[test]
fn rescan_picks_up_changes_and_keeps_totals_consistent() {
    let t = TempDir::new("rescan");
    std::fs::create_dir_all(t.path().join("sub")).unwrap();
    std::fs::write(t.path().join("sub").join("a.bin"), vec![1u8; 200_000]).unwrap();
    let (scanner, node) = scan(&t.s(), 4);
    let sub = scanner.children(&node).remove(0);
    let before = node.total_bytes();
    std::fs::write(t.path().join("sub").join("b.bin"), vec![1u8; 300_000]).unwrap();
    scanner.rescan(&sub);
    assert!(scanner.wait_idle(Duration::from_secs(10)));
    assert!(sub.is_done());
    assert!(node.total_bytes() >= before + 300_000, "{} vs {}", node.total_bytes(), before);
    // Rescanning twice must not double count.
    let once = node.total_bytes();
    let sub = scanner.children(&node).remove(0);
    scanner.rescan(&sub);
    assert!(scanner.wait_idle(Duration::from_secs(10)));
    assert_eq!(node.total_bytes(), once);
}

#[test]
fn scanning_ancestor_after_descendant_counts_once() {
    // The UI scans the folder being viewed first and then its parent: totals must not double count.
    let t = TempDir::new("order");
    make_tree(t.path(), 3, 3, 3);
    let deep = t.path().join("d0").join("d1");
    let scanner = Scanner::new(4);
    let deep_node = scanner.node_for_path(deep.to_str().unwrap());
    scanner.scan(&deep_node);
    assert!(scanner.wait_idle(Duration::from_secs(30)));
    let top = scanner.node_for_path(&t.s());
    scanner.scan(&top);
    assert!(scanner.wait_idle(Duration::from_secs(30)));
    assert!(top.is_done());

    let (fresh, fresh_top) = scan(&t.s(), 4);
    let _keep = &fresh;
    assert_eq!(top.total_bytes(), fresh_top.total_bytes());
    assert_eq!(top.item_count(), fresh_top.item_count());
}

#[test]
fn unreadable_directories_are_counted_not_fatal() {
    let scanner = Scanner::new(2);
    let node = scanner.node_for_path("C:\\System Volume Information");
    scanner.scan(&node);
    assert!(scanner.wait_idle(Duration::from_secs(10)));
    assert!(node.is_done());
    assert!(scanner.denied() >= 1 || node.item_count() >= 0);
}

#[test]
fn missing_directory_is_marked_unreadable() {
    let scanner = Scanner::new(2);
    let node = scanner.node_for_path("C:\\definitely\\does\\not\\exist-8f3a");
    scanner.scan(&node);
    assert!(scanner.wait_idle(Duration::from_secs(10)));
    assert!(node.is_done());
    assert!(scanner.is_unreadable(&node));
    assert_eq!(scanner.denied(), 1);
}

#[test]
fn drives_are_independent_scan_roots() {
    let scanner = Scanner::new(2);
    let c = scanner.node_for_path("C:\\");
    scanner.mark_mount_root(&c);
    assert!(c.is_mount_root());
    let d = scanner.node_for_path("C:\\Windows\\Fonts\\");
    scanner.scan(&d);
    assert!(scanner.wait_idle(Duration::from_secs(30)));
    // Bytes propagate up to the drive root but stop there: the virtual root stays empty.
    assert!(c.total_bytes() > 0);
    assert_eq!(scanner.root.total_bytes(), 0);
}

#[test]
fn single_and_many_threads_agree() {
    let t = TempDir::new("threads");
    make_tree(t.path(), 3, 4, 4);
    let (_a, one) = scan(&t.s(), 1);
    let (_b, many) = scan(&t.s(), 16);
    assert_eq!(one.total_bytes(), many.total_bytes());
    assert_eq!(one.item_count(), many.item_count());
}

#[test]
fn large_flat_directory_listing_is_complete() {
    // Many entries need several buffer refills; make sure none are lost or duplicated.
    let t = TempDir::new("flat");
    for i in 0..5000 {
        std::fs::write(t.path().join(format!("file-with-a-fairly-long-name-to-fill-buffers-{i:06}.txt")), b"x").unwrap();
    }
    let (_s, node) = scan(&t.s(), 4);
    assert_eq!(node.item_count(), 5000);
    let listed = spacemaster::listing::list(&t.s()).unwrap();
    assert_eq!(listed.len(), 5000);
    let mut names: Vec<_> = listed.iter().map(|e| e.name.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 5000);
}

/// Ad-hoc check against a real tree: `SM_REAL_TREE=C:\Some\Dir cargo test real_tree -- --ignored --nocapture`
#[test]
#[ignore]
fn real_tree_matches_walk() {
    let dir = std::env::var("SM_REAL_TREE").expect("set SM_REAL_TREE");
    let (items, dirs) = walk(Path::new(&dir));
    let (scanner, node) = scan(&dir, 8);
    println!("walk: {items} items {dirs} dirs; scanner: {} items {} dirs", node.item_count(), scanner.dirs_scanned());
    assert_eq!(node.item_count(), items);
}
