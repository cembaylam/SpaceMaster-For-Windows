//! Browser controller tests: navigation, keys, sorting, filtering, selection, deleting and undo,
//! all on a throw-away "home" folder.

use spacemaster::browser::{Browser, DeleteOutcome, Event, Key, KeyEvent, PromptKind};
use spacemaster::model::{Model, SortKey};
use spacemaster::safety::{Level, SafetyClassifier};
use spacemaster::settings::Settings;
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Home {
    root: PathBuf,
    dir: PathBuf,
}

impl Home {
    /// `<target>/tmp/sm-browser-<pid>-<name>/home`: the parent folder (which the browser scans too) stays tiny.
    fn new(name: &str) -> Home {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("sm-browser-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let d = root.join("home");
        std::fs::create_dir_all(&d).unwrap();
        // Canonical spelling (long names, drive case) so paths compare equal to what the browser reports.
        let c = std::fs::canonicalize(&d).unwrap();
        let s = c.to_string_lossy().trim_start_matches(r"\\?\").to_string();
        Home { root, dir: PathBuf::from(s) }
    }
    fn s(&self) -> String {
        self.dir.to_string_lossy().into_owned()
    }
    fn write(&self, rel: &str, bytes: usize) {
        let p = self.dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![9u8; bytes]).unwrap();
    }
    fn model(&self, threads: usize) -> std::sync::Arc<Model> {
        let model = Model::with(threads, SafetyClassifier::with_layout(&self.s(), "C:", |_| true));
        model.scan_drive.store(false, std::sync::atomic::Ordering::Relaxed);
        model
    }
    fn browser(&self) -> Browser {
        let mut b = Browser::new(self.model(4), Settings::memory());
        b.visible_rows = 10;
        settle(&mut b);
        b
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn settle(b: &mut Browser) {
    assert!(b.model.scanner.wait_idle(Duration::from_secs(60)), "scan did not finish");
    b.poll();
    b.refresh_visible();
}

fn names(b: &Browser) -> Vec<String> {
    b.current.borrow().rows.iter().map(|r| r.name.clone()).collect()
}

fn keys(b: &mut Browser, s: &str) {
    for c in s.chars() {
        b.handle_key(KeyEvent::ch(c));
    }
}

fn take_alert(b: &mut Browser) -> Option<(String, String)> {
    let mut found = None;
    let mut rest = Vec::new();
    for e in b.events.drain(..) {
        match e {
            Event::Alert { title, text } if found.is_none() => found = Some((title, text)),
            other => rest.push(other),
        }
    }
    b.events = rest;
    found
}

fn standard(h: &Home) {
    h.write("big\\data.bin", 3_000_000);
    h.write("medium\\data.bin", 1_000_000);
    h.write("small\\x.bin", 50_000);
    h.write("file10.txt", 10);
    h.write("file2.txt", 20);
    h.write("a.txt", 5);
}

#[test]
fn starts_in_home_sorted_by_size_with_live_sizes() {
    let h = Home::new("start");
    standard(&h);
    let b = h.browser();
    assert_eq!(b.cwd, h.s());
    let n = names(&b);
    assert_eq!(&n[..3], ["big", "medium", "small"], "{n:?}");
    // Percent denominators: total is the sum of all rows.
    let s = b.current.borrow();
    assert_eq!(s.total, s.rows.iter().map(|r| r.size).sum::<i64>());
    assert!(!s.any_calculating);
    assert!(s.rows[0].size >= 3_000_000);
    assert_eq!(s.max_size, s.rows[0].size);
}

#[test]
fn navigation_cursor_memory_and_history() {
    let h = Home::new("nav");
    standard(&h);
    let mut b = h.browser();
    keys(&mut b, "j"); // medium
    assert_eq!(b.cursor_name.as_deref(), Some("medium"));
    keys(&mut b, "l"); // enter medium
    assert_eq!(b.cwd, format!("{}\\medium", h.s()));
    assert_eq!(names(&b), ["data.bin"]);
    keys(&mut b, "h"); // back to home, cursor stays on medium
    assert_eq!(b.cwd, h.s());
    assert_eq!(b.cursor_name.as_deref(), Some("medium"));
    // History.
    keys(&mut b, "H");
    assert_eq!(b.cwd, format!("{}\\medium", h.s()));
    assert!(b.can_go_forward());
    keys(&mut b, "L");
    assert_eq!(b.cwd, h.s());
    assert!(b.can_go_back());
    // Arrow keys and Backspace.
    b.handle_key(KeyEvent::new(Key::Right));
    assert_eq!(b.cwd, format!("{}\\medium", h.s()));
    b.handle_key(KeyEvent::new(Key::Backspace));
    assert_eq!(b.cwd, h.s());
    // Alt+Left goes back in history.
    let mut ev = KeyEvent::new(Key::Left);
    ev.alt = true;
    b.handle_key(ev);
    assert_eq!(b.cwd, format!("{}\\medium", h.s()));
}

#[test]
fn cursor_movement_keys() {
    let h = Home::new("cursor");
    standard(&h);
    let mut b = h.browser();
    let n = names(&b);
    assert_eq!(n.len(), 6);
    keys(&mut b, "G");
    assert_eq!(b.cursor_name.as_deref(), Some(n[5].as_str()));
    keys(&mut b, "gg");
    assert_eq!(b.cursor_name.as_deref(), Some(n[0].as_str()));
    keys(&mut b, "jjj");
    assert_eq!(b.cursor_index(), 3);
    keys(&mut b, "k");
    assert_eq!(b.cursor_index(), 2);
    b.handle_key(KeyEvent::new(Key::End));
    assert_eq!(b.cursor_index(), 5);
    b.handle_key(KeyEvent::new(Key::Home));
    assert_eq!(b.cursor_index(), 0);
    b.handle_key(KeyEvent::new(Key::PageDown));
    assert_eq!(b.cursor_index(), 5); // clamped
    b.handle_key(KeyEvent::new(Key::Up));
    assert_eq!(b.cursor_index(), 4);
    // Half page = visible_rows(10) - 1 = 9 / 2 = 4.
    b.handle_key(KeyEvent::ctrl('u'));
    assert_eq!(b.cursor_index(), 0);
    b.handle_key(KeyEvent::ctrl('d'));
    assert_eq!(b.cursor_index(), 4);
    // Unknown keys are not handled.
    assert!(!b.handle_key(KeyEvent::ch('~')));
}

#[test]
fn sorting_keys_and_natural_name_order() {
    let h = Home::new("sort");
    standard(&h);
    let mut b = h.browser();
    keys(&mut b, ",n");
    assert_eq!(b.sort, SortKey::Name);
    assert_eq!(names(&b), ["a.txt", "big", "file2.txt", "file10.txt", "medium", "small"]);
    keys(&mut b, ",N"); // reversed
    assert!(b.sort_reversed);
    assert_eq!(names(&b)[0], "small");
    keys(&mut b, ",s");
    assert_eq!(names(&b)[..3], ["big", "medium", "small"]);
    keys(&mut b, ",S");
    assert_eq!(names(&b).last().unwrap(), "big");
    // Date: touch a file to be newest.
    keys(&mut b, ",m");
    assert_eq!(b.sort, SortKey::Modified);
    assert!(!b.sort_reversed);
    // Menu style toggling reverses the active key.
    b.sort_by(SortKey::Modified);
    assert!(b.sort_reversed);
    // Persisted.
    assert_eq!(b.settings.get("sort"), Some("modified"));
}

#[test]
fn filter_prompt_filters_live_and_escape_clears() {
    let h = Home::new("filter");
    standard(&h);
    let mut b = h.browser();
    keys(&mut b, "/");
    assert_eq!(b.prompt.as_ref().unwrap().kind, PromptKind::Filter);
    keys(&mut b, "FILE");
    assert_eq!(names(&b).len(), 2, "{:?}", names(&b));
    assert!(names(&b).iter().all(|n| n.starts_with("file")));
    b.handle_key(KeyEvent::new(Key::Enter)); // keeps the filter, closes the prompt
    assert!(b.prompt.is_none());
    assert_eq!(names(&b).len(), 2);
    assert!(b.summary().starts_with("2 items"));
    b.handle_key(KeyEvent::new(Key::Escape)); // normal mode Esc clears the filter
    assert!(b.filter.is_empty());
    assert_eq!(names(&b).len(), 6);
    // Esc inside the prompt also clears.
    keys(&mut b, "f");
    keys(&mut b, "zzz");
    assert!(names(&b).is_empty());
    b.handle_key(KeyEvent::new(Key::Escape));
    assert!(b.prompt.is_none());
    assert_eq!(names(&b).len(), 6);
    // Backspace edits the filter.
    keys(&mut b, "/ab");
    b.handle_key(KeyEvent::new(Key::Backspace));
    assert_eq!(b.prompt.as_ref().unwrap().text, "a");
    // Changing folders drops the filter.
    b.handle_key(KeyEvent::new(Key::Enter));
    assert_eq!(b.filter, "a");
    keys(&mut b, "gh");
    assert!(b.filter.is_empty());
}

#[test]
fn hidden_files_toggle() {
    let h = Home::new("hidden");
    standard(&h);
    h.write("secret.txt", 10);
    let status = std::process::Command::new("attrib").arg("+h").arg(h.dir.join("secret.txt")).status().unwrap();
    assert!(status.success());
    let mut b = h.browser();
    assert!(b.show_hidden);
    assert!(names(&b).contains(&"secret.txt".to_string()));
    keys(&mut b, ".");
    assert!(!b.show_hidden);
    assert!(!names(&b).contains(&"secret.txt".to_string()));
    assert!(b.summary().contains("1 hidden"), "{}", b.summary());
    keys(&mut b, ".");
    assert!(names(&b).contains(&"secret.txt".to_string()));
    let row = b.current.borrow().rows.iter().find(|r| r.name == "secret.txt").cloned().unwrap();
    assert!(row.hidden);
}

#[test]
fn selection_and_summary() {
    let h = Home::new("select");
    standard(&h);
    let mut b = h.browser();
    b.handle_key(KeyEvent::new(Key::Tab)); // select big, advance
    b.handle_key(KeyEvent::new(Key::Tab)); // select medium, advance
    assert_eq!(b.selection.len(), 2);
    assert_eq!(b.cursor_name.as_deref(), Some("small"));
    assert!(b.summary().starts_with("2 selected"), "{}", b.summary());
    // Tab again on a selected row deselects.
    keys(&mut b, "k");
    b.handle_key(KeyEvent::new(Key::Tab));
    assert_eq!(b.selection.len(), 1);
    b.handle_key(KeyEvent::new(Key::Escape));
    assert!(b.selection.is_empty());
    keys(&mut b, "v");
    assert_eq!(b.selection.len(), 6);
    b.handle_key(KeyEvent::new(Key::Escape));
    b.handle_key(KeyEvent::ctrl('a'));
    assert_eq!(b.selection.len(), 6);
    // Mouse: ctrl-click toggles, shift-click extends, plain click clears.
    b.handle_key(KeyEvent::new(Key::Escape));
    b.click_row(1, false, false);
    b.click_row(3, false, true);
    assert_eq!(b.selection.len(), 3);
    b.click_row(5, true, false);
    assert_eq!(b.selection.len(), 4);
    b.click_row(0, false, false);
    assert!(b.selection.is_empty());
    assert_eq!(b.cursor_index(), 0);
}

#[test]
fn space_opens_the_viewer_and_no_longer_marks()  {
    let h = Home::new("space");
    standard(&h);
    let mut b = h.browser();
    b.events.clear();
    b.cursor_name = Some("a.txt".into());
    keys(&mut b, " ");
    assert!(matches!(b.events.last(), Some(Event::OpenViewer(p)) if p == &format!("{}\\a.txt", h.s())));
    assert!(b.selection.is_empty(), "Space must not mark files");
    // On a folder Space does nothing.
    b.events.clear();
    b.cursor_name = Some("big".into());
    keys(&mut b, " ");
    assert!(b.events.is_empty());
}

#[test]
fn go_to_prompt_accepts_typed_paths() {
    let h = Home::new("goto");
    standard(&h);
    let mut b = h.browser();
    b.handle_key(KeyEvent::ctrl('l'));
    let p = b.prompt.clone().unwrap();
    assert_eq!(p.kind, PromptKind::GoTo);
    assert_eq!(p.text, format!("{}\\", h.s()));
    // Ctrl+Backspace removes a whole path component; then type a new one (case-insensitive) and confirm.
    for _ in 0..p.text.len() {
        b.handle_key(KeyEvent::new(Key::Backspace));
    }
    b.prompt_insert(&format!("{}\\MEDIUM", h.s().to_lowercase()));
    b.handle_key(KeyEvent::new(Key::Enter));
    assert_eq!(b.cwd, format!("{}\\medium", h.s()));
    // "~" expands to the home folder.
    keys(&mut b, "g ");
    b.prompt_insert("~");
    // the prompt started with the cwd; clear it first
    b.handle_key(KeyEvent::ctrl('u'));
    b.prompt_insert("~");
    b.handle_key(KeyEvent::new(Key::Enter));
    assert_eq!(b.cwd, h.s());
    // Bad paths keep the location and flash a message.
    b.handle_key(KeyEvent::ctrl('l'));
    b.handle_key(KeyEvent::ctrl('u'));
    b.prompt_insert("Z:\\no\\such\\folder");
    b.handle_key(KeyEvent::new(Key::Enter));
    assert_eq!(b.cwd, h.s());
    assert!(b.flash.as_ref().unwrap().text.contains("Can't open"));
}

#[test]
fn this_pc_lists_drives() {
    let h = Home::new("thispc");
    standard(&h);
    let mut b = h.browser();
    assert!(b.cd("", None, true));
    assert_eq!(b.cwd, "");
    let s = b.current.borrow();
    assert!(!s.rows.is_empty());
    assert!(s.rows.iter().all(|r| r.is_dir() && r.name.ends_with(':')));
    // Used space is shown immediately.
    assert!(s.rows.iter().any(|r| r.size > 0));
    // Drives are never deletable.
    assert!(s.rows.iter().all(|r| r.safety.level != Level::Safe));
    drop(s);
    // Entering a drive and leaving it returns to This PC with the cursor on that drive.
    let drive = b.cursor_row().unwrap().name;
    b.enter();
    assert_eq!(b.cwd, format!("{drive}\\"));
    b.leave();
    assert_eq!(b.cwd, "");
    assert_eq!(b.cursor_name.as_deref(), Some(drive.as_str()));
    // `gr` jumps to This PC.
    keys(&mut b, "gh");
    keys(&mut b, "gr");
    assert_eq!(b.cwd, "");
}

#[test]
fn protected_items_cannot_be_deleted() {
    let h = Home::new("protected");
    standard(&h);
    h.write("Documents\\report.docx", 100);
    h.write(".ssh\\id_rsa", 100);
    let mut b = h.browser();
    b.cursor_name = Some("Documents".into());
    keys(&mut b, "d");
    let (title, text) = take_alert(&mut b).expect("alert for protected folder");
    assert!(title.contains("Documents"), "{title}");
    assert!(text.contains("Delete its contents instead"), "{text}");
    assert_eq!(b.deleting, 0);
    // Mixed folders are flagged as such and can't be deleted either.
    let r = b.current.borrow().rows.iter().find(|r| r.name == "Documents").cloned().unwrap();
    assert_eq!(r.safety.level, Level::Mixed);
    let r = b.current.borrow().rows.iter().find(|r| r.name == ".ssh").cloned().unwrap();
    assert_eq!(r.safety.level, Level::Protected);
    // A selection containing a protected item is refused as a whole.
    b.select_all();
    keys(&mut b, "D");
    let (title, _) = take_alert(&mut b).expect("alert");
    assert!(title.contains("protected"), "{title}");
    assert!(!b.events.iter().any(|e| matches!(e, Event::ConfirmDelete(_) | Event::RunDelete(_))));
    assert!(h.dir.join("big").exists());
}

fn take_delete(b: &mut Browser) -> (spacemaster::browser::DeleteRequest, bool) {
    let mut out = None;
    let mut rest = Vec::new();
    for e in b.events.drain(..) {
        match e {
            Event::ConfirmDelete(r) => out = Some((r, true)),
            Event::RunDelete(r) => out = Some((r, false)),
            other => rest.push(other),
        }
    }
    b.events = rest;
    out.expect("delete event")
}

#[test]
fn permanent_delete_asks_first_and_updates_sizes() {
    let h = Home::new("permdel");
    standard(&h);
    let mut b = h.browser();
    let before = b.current.borrow().total;
    let big = b.current.borrow().rows.iter().find(|r| r.name == "big").map(|r| r.size).unwrap();
    assert_eq!(b.cursor_name.as_deref(), Some("big"));
    keys(&mut b, "D");
    let (req, needs_confirm) = take_delete(&mut b);
    assert!(needs_confirm, "permanent deletes must be confirmed");
    assert_eq!(req.bytes, big);
    assert!(h.dir.join("big").exists(), "nothing is deleted before the user confirms");
    b.start_delete(req);
    let (req, needs_confirm) = take_delete(&mut b);
    assert!(!needs_confirm);
    assert_eq!(b.deleting, 1);
    b.delete_sync(req);
    // (delete_sync increments again; finish_delete saturates the counter back)
    b.deleting = 0;
    assert!(!h.dir.join("big").exists());
    assert_eq!(b.current.borrow().total, before - big);
    assert!(!names(&b).contains(&"big".to_string()));
    // Nothing to undo after a permanent delete.
    assert!(!b.can_undo());
    assert!(b.flash.as_ref().unwrap().text.starts_with("Deleted 1 item"));
    // The cursor moved on to a neighbour.
    assert!(b.cursor_name.is_some());
}

#[test]
fn trash_and_undo_roundtrip() {
    let h = Home::new("trash");
    standard(&h);
    let mut b = h.browser();
    let before = b.current.borrow().total;
    b.cursor_name = Some("medium".into());
    b.update_preview();
    keys(&mut b, "d"); // trash needs no confirmation
    let (req, needs_confirm) = take_delete(&mut b);
    assert!(!needs_confirm);
    let outcome = spacemaster::browser::run_delete(&req, 0);
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    b.finish_delete(req, outcome);
    assert!(!h.dir.join("medium").exists());
    assert!(!names(&b).contains(&"medium".to_string()));
    assert!(b.can_undo());
    assert!(b.current.borrow().total < before);
    b.handle_key(KeyEvent::ctrl('z'));
    assert!(h.dir.join("medium").join("data.bin").exists(), "restored from the Recycle Bin");
    assert!(!b.can_undo());
    settle(&mut b);
    b.app_activated();
    settle(&mut b);
    assert!(names(&b).contains(&"medium".to_string()));
    assert_eq!(b.current.borrow().total, before);
}

#[test]
fn failed_deletes_are_reported_and_other_items_still_go() {
    let h = Home::new("failed");
    standard(&h);
    let mut b = h.browser();
    b.cursor_name = Some("a.txt".into());
    b.selection.insert("a.txt".into());
    b.selection.insert("file2.txt".into());
    keys(&mut b, "D");
    let (mut req, _) = take_delete(&mut b);
    // Someone else removed one of the files in the meantime.
    std::fs::remove_file(h.dir.join("a.txt")).unwrap();
    req.permanently = true;
    b.deleting = 1;
    let outcome = spacemaster::browser::run_delete(&req, 0);
    assert_eq!(outcome.done.len(), 1);
    assert_eq!(outcome.failed.len(), 1);
    b.finish_delete(req, DeleteOutcome { done: outcome.done, failed: outcome.failed });
    let (title, text) = take_alert(&mut b).expect("failure alert");
    assert!(title.contains("1 item couldn't be deleted"), "{title}");
    assert!(text.contains("a.txt"), "{text}");
    assert!(!h.dir.join("file2.txt").exists());
}

#[test]
fn rescan_picks_up_external_changes() {
    let h = Home::new("rescan");
    standard(&h);
    let mut b = h.browser();
    let before = b.current.borrow().total;
    h.write("big\\more.bin", 2_000_000);
    keys(&mut b, "r"); // cursor is on "big": rescan that folder
    settle(&mut b);
    b.app_activated();
    settle(&mut b);
    assert!(b.current.borrow().total >= before + 2_000_000);
}

#[test]
fn actions_emit_events() {
    let h = Home::new("events");
    standard(&h);
    let mut b = h.browser();
    b.events.clear();
    keys(&mut b, "y");
    assert!(matches!(b.events.last(), Some(Event::CopyText(t)) if t == &format!("{}\\big", h.s())));
    keys(&mut b, "R");
    assert!(matches!(b.events.last(), Some(Event::Reveal(p)) if p == &vec![format!("{}\\big", h.s())]));
    keys(&mut b, "?");
    assert!(matches!(b.events.last(), Some(Event::ShowHelp)));
    keys(&mut b, "gt");
    assert!(matches!(b.events.last(), Some(Event::OpenRecycleBin)));
    b.cursor_name = Some("a.txt".into());
    b.handle_key(KeyEvent::new(Key::Enter));
    assert!(matches!(b.events.last(), Some(Event::Open(p)) if p == &format!("{}\\a.txt", h.s())));
    // Ctrl+C copies the files themselves.
    b.handle_key(KeyEvent::ctrl('c'));
    assert!(matches!(b.events.last(), Some(Event::CopyFiles(p)) if p.len() == 1));
}

#[test]
fn settings_are_restored() {
    let h = Home::new("settings");
    standard(&h);
    h.write("sub\\x.bin", 10);
    let mut s = Settings::memory();
    s.set("sort", "name");
    s.set("sortReversed", true);
    s.set("showHidden", false);
    s.set("lastPath", format!("{}\\sub", h.s()));
    let model = h.model(2);
    let b = Browser::new(model, s);
    assert_eq!(b.sort, SortKey::Name);
    assert!(b.sort_reversed);
    assert!(!b.show_hidden);
    assert_eq!(b.cwd, format!("{}\\sub", h.s()));
    // A vanished last path falls back to home.
    let mut s = Settings::memory();
    s.set("lastPath", "Z:\\gone");
    let model = h.model(2);
    let b = Browser::new(model, s);
    assert_eq!(b.cwd, h.s());
}

#[test]
fn empty_folder_and_unreadable_folder_states() {
    let h = Home::new("empty");
    std::fs::create_dir_all(h.dir.join("nothing")).unwrap();
    let mut b = h.browser();
    b.enter();
    assert!(names(&b).is_empty());
    assert_eq!(b.current.borrow().error, None);
    assert_eq!(b.summary(), "0 items, 0 B");
    // Keys on an empty folder must not panic.
    keys(&mut b, "jkGdDyRl");
    b.handle_key(KeyEvent::new(Key::Enter));
    assert!(b.cursor_row().is_none());
    // A folder we can't read shows an error instead of rows.
    let sd = Path::new("C:\\System Volume Information");
    if sd.exists() {
        b.cd("C:\\System Volume Information", None, true);
        assert!(b.current.borrow().error.as_deref().is_some_and(|e| e.contains("permission")));
    }
}
