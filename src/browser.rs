//! The browser controller: three Miller columns (parent | current | preview), cursor, selection, history,
//! sorting, filtering, yazi/vim style keys, deleting and undo. It has no UI code; the Direct2D front end renders
//! its state and feeds it key events, and reacts to the [`Event`]s it queues.

use crate::format;
use crate::fsops;
use crate::listing::Kind;
use crate::model::{DirState, Model, Row, SortKey, StateRef};
use crate::node::Node;
use crate::paths;
use crate::settings::Settings;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    Backspace,
    Delete,
    Tab,
    PageUp,
    PageDown,
    Home,
    End,
    F5,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl KeyEvent {
    pub fn new(key: Key) -> Self {
        KeyEvent { key, ctrl: false, shift: false, alt: false }
    }
    pub fn ch(c: char) -> Self {
        Self::new(Key::Char(c))
    }
    pub fn ctrl(c: char) -> Self {
        KeyEvent { key: Key::Char(c), ctrl: true, shift: false, alt: false }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PromptKind {
    Filter,
    GoTo,
}

/// Single line text input (filter / go to path) that the front end draws in the path bar.
#[derive(Clone, Debug)]
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
}

pub struct DeleteItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: i64,
    pub node: Option<Arc<Node>>,
}

pub struct DeleteRequest {
    pub items: Vec<DeleteItem>,
    pub permanently: bool,
    pub dir_path: String,
    pub dir_node: Arc<Node>,
    pub bytes: i64,
}

pub struct DeleteOutcome {
    pub done: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Things the front end has to do on behalf of the controller.
pub enum Event {
    Open(String),
    /// Space: the built-in viewer for this file.
    OpenViewer(String),
    Reveal(Vec<String>),
    CopyText(String),
    CopyFiles(Vec<String>),
    OpenRecycleBin,
    ShowHelp,
    Alert { title: String, text: String },
    /// Ask before permanently deleting; on yes call [`Browser::start_delete`].
    ConfirmDelete(DeleteRequest),
    /// Run [`run_delete`] off the UI thread, then call [`Browser::finish_delete`].
    RunDelete(DeleteRequest),
    LocationChanged,
}

struct TrashRecord {
    original: String,
    is_dir: bool,
    size: i64,
}

pub struct Flash {
    pub text: String,
    pub until: Instant,
}

pub struct Browser {
    pub model: Arc<Model>,
    pub settings: Settings,
    pub cwd: String,
    pub current: StateRef,
    pub parent: Option<StateRef>,
    pub preview: Option<StateRef>,
    cursor_memory: HashMap<String, String>,
    pub cursor_name: Option<String>,
    /// Cursor follows the top row while sizes are still arriving and the user hasn't moved it.
    pub pinned_to_top: bool,
    pub selection: HashSet<String>,
    history: Vec<String>,
    history_index: isize,
    states: HashMap<String, StateRef>,
    state_order: Vec<String>,
    pub sort: SortKey,
    pub sort_reversed: bool,
    pub show_hidden: bool,
    pub filter: String,
    pub prompt: Option<Prompt>,
    pending_prefix: Option<char>,
    last_version: u64,
    undo_stack: Vec<Vec<TrashRecord>>,
    pub deleting: u32,
    pub flash: Option<Flash>,
    pub events: Vec<Event>,
    /// Rows visible in the current column (set by the front end; used for paging).
    pub visible_rows: usize,
    /// Bumped on every change the front end should repaint for.
    pub revision: u64,
}

const CACHE_LIMIT: usize = 40;
const CACHE_TTL: Duration = Duration::from_secs(15);

impl Browser {
    pub fn new(model: Arc<Model>, settings: Settings) -> Browser {
        let sort = settings.get("sort").and_then(SortKey::from_id).unwrap_or(SortKey::Size);
        let sort_reversed = settings.get_bool("sortReversed", false);
        let show_hidden = settings.get_bool("showHidden", true);
        let start = settings
            .get("lastPath")
            .and_then(|p| model.resolve(p))
            .unwrap_or_else(|| model.home.clone());
        model.start_scanning(&start);
        let current = Rc::new(RefCell::new(DirState::load(&model, &start)));
        let mut b = Browser {
            model,
            settings,
            cwd: start.clone(),
            current,
            parent: None,
            preview: None,
            cursor_memory: HashMap::new(),
            cursor_name: None,
            pinned_to_top: false,
            selection: HashSet::new(),
            history: Vec::new(),
            history_index: -1,
            states: HashMap::new(),
            state_order: Vec::new(),
            sort,
            sort_reversed,
            show_hidden,
            filter: String::new(),
            prompt: None,
            pending_prefix: None,
            last_version: u64::MAX,
            undo_stack: Vec::new(),
            deleting: 0,
            flash: None,
            events: Vec::new(),
            visible_rows: 30,
            revision: 0,
        };
        b.cd(&start, None, true);
        b
    }

    fn touch(&mut self) {
        self.revision += 1;
    }

    // MARK: State loading

    fn load_state(&mut self, path: &str, force: bool) -> StateRef {
        if !force {
            if let Some(s) = self.states.get(path) {
                if s.borrow().loaded_at.elapsed() < CACHE_TTL {
                    return s.clone();
                }
            }
        }
        let mut st = DirState::load(&self.model, path);
        let filter = if path == self.cwd { self.filter.clone() } else { String::new() };
        st.rebuild(&self.model, self.sort, self.sort_reversed, self.show_hidden, &filter);
        let s = Rc::new(RefCell::new(st));
        self.states.insert(path.to_string(), s.clone());
        self.state_order.retain(|p| p != path);
        self.state_order.push(path.to_string());
        if self.state_order.len() > CACHE_LIMIT {
            let old = self.state_order.remove(0);
            self.states.remove(&old);
        }
        s
    }

    fn invalidate_cache(&mut self) {
        self.states.clear();
        self.state_order.clear();
    }

    fn reload_columns(&mut self) {
        let cwd = self.cwd.clone();
        self.current = self.load_state(&cwd, true);
        self.parent = paths::parent(&cwd).map(|p| self.load_state(&p, true));
    }

    // MARK: Navigation

    /// Changes directory. `raw` may be a typed path. Returns false (with a flash message) when impossible.
    pub fn cd(&mut self, raw: &str, focus: Option<String>, add_history: bool) -> bool {
        let Some(path) = self.model.resolve(raw) else {
            self.set_flash(format!("Can't open \u{201c}{}\u{201d}", raw.trim()), 5);
            return false;
        };
        if self.history_index >= 0 || !self.cwd.is_empty() || !self.history.is_empty() {
            if let Some(n) = self.cursor_name.clone() {
                self.cursor_memory.insert(self.cwd.clone(), n);
            }
        }
        if add_history && (path != self.cwd || self.history.is_empty()) {
            let keep = (self.history_index + 1).max(0) as usize;
            self.history.truncate(keep);
            self.history.push(path.clone());
            self.history_index = self.history.len() as isize - 1;
        }
        self.cwd = path.clone();
        self.selection.clear();
        self.filter.clear();
        self.prompt = None;
        self.pending_prefix = None;
        self.settings.set("lastPath", &path);

        // Scan the parent first and the folder itself on top: the folder being viewed always gets scanned first.
        if let Some(p) = paths::parent(&path) {
            self.model.scan(&p);
        }
        self.model.scan(&path);

        self.reload_columns();
        let first = self.current.borrow().rows.first().map(|r| r.name.clone());
        let remembered = self.cursor_memory.get(&path).cloned();
        self.pinned_to_top = focus.is_none() && remembered.is_none();
        self.cursor_name = focus.or(remembered).or(first.clone());
        if self.current.borrow().index_of(self.cursor_name.as_deref()).is_none() {
            self.cursor_name = first;
        }
        self.update_preview();
        self.events.push(Event::LocationChanged);
        self.touch();
        true
    }

    pub fn go_back(&mut self) {
        if self.history_index > 0 {
            self.history_index -= 1;
            let p = self.history[self.history_index as usize].clone();
            self.cd(&p, None, false);
        }
    }

    pub fn go_forward(&mut self) {
        if self.history_index < self.history.len() as isize - 1 {
            self.history_index += 1;
            let p = self.history[self.history_index as usize].clone();
            self.cd(&p, None, false);
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.history_index > 0
    }
    pub fn can_go_forward(&self) -> bool {
        self.history_index < self.history.len() as isize - 1
    }

    pub fn leave(&mut self) {
        if let Some(p) = paths::parent(&self.cwd) {
            let name = paths::file_name(&self.cwd).to_string();
            self.cd(&p, Some(name), true);
        }
    }

    pub fn cursor_row(&self) -> Option<Row> {
        let s = self.current.borrow();
        s.index_of(self.cursor_name.as_deref()).map(|i| s.rows[i].clone())
    }

    pub fn cursor_index(&self) -> usize {
        self.current.borrow().index_of(self.cursor_name.as_deref()).unwrap_or(0)
    }

    pub fn path_of(&self, name: &str) -> String {
        paths::join(&self.cwd, name)
    }

    pub fn enter(&mut self) {
        if let Some(r) = self.cursor_row() {
            if r.is_dir() || r.kind == Kind::Symlink && r.attrs & 0x10 != 0 {
                let p = self.path_of(&r.name);
                self.cd(&p, None, true);
            }
        }
    }

    /// Space: opens the built-in viewer on the file under the cursor (folders: nothing).
    pub fn open_viewer(&mut self) {
        if let Some(r) = self.cursor_row() {
            if !r.is_dir() {
                let p = self.path_of(&r.name);
                self.events.push(Event::OpenViewer(p));
            }
        }
    }

    pub fn open_cursor(&mut self) {
        if let Some(r) = self.cursor_row() {
            let p = self.path_of(&r.name);
            if r.is_dir() || r.kind == Kind::Symlink && r.attrs & 0x10 != 0 {
                self.cd(&p, None, true);
            } else {
                self.events.push(Event::Open(p));
            }
        }
    }

    pub fn move_cursor(&mut self, index: isize) {
        let name = {
            let s = self.current.borrow();
            if s.rows.is_empty() {
                return;
            }
            let i = index.clamp(0, s.rows.len() as isize - 1) as usize;
            s.rows[i].name.clone()
        };
        self.cursor_name = Some(name);
        self.pinned_to_top = false;
        self.update_preview();
        self.touch();
    }

    pub fn move_cursor_by(&mut self, delta: isize) {
        let i = self.cursor_index() as isize;
        self.move_cursor(i.saturating_add(delta));
    }

    // MARK: Mouse helpers (front end)

    /// Click on a row of the current column.
    pub fn click_row(&mut self, index: usize, ctrl: bool, shift: bool) {
        let name = {
            let s = self.current.borrow();
            match s.rows.get(index) {
                Some(r) => r.name.clone(),
                None => return,
            }
        };
        if ctrl {
            if !self.selection.remove(&name) {
                self.selection.insert(name);
            }
        } else if shift {
            if let Some(a) = self.current.borrow().index_of(self.cursor_name.as_deref()) {
                let s = self.current.borrow();
                for j in a.min(index)..=a.max(index) {
                    self.selection.insert(s.rows[j].name.clone());
                }
            }
        } else if !self.selection.is_empty() {
            self.selection.clear();
        }
        self.move_cursor(index as isize);
    }

    pub fn click_parent_row(&mut self, index: usize) {
        let target = match &self.parent {
            Some(p) => {
                let p = p.borrow();
                p.rows.get(index).filter(|r| r.is_dir()).map(|r| paths::join(&p.path, &r.name))
            }
            None => None,
        };
        if let Some(t) = target {
            self.cd(&t, None, true);
        }
    }

    pub fn click_preview_row(&mut self, index: usize) {
        let target = match &self.preview {
            Some(p) => {
                let p = p.borrow();
                p.rows.get(index).filter(|r| r.is_dir()).map(|r| paths::join(&p.path, &r.name))
            }
            None => None,
        };
        if let Some(t) = target {
            self.cd(&t, None, true);
        }
    }

    // MARK: Preview

    pub fn update_preview(&mut self) {
        let Some(r) = self.cursor_row() else {
            self.preview = None;
            return;
        };
        if r.is_dir() {
            let p = self.path_of(&r.name);
            self.model.scan(&p);
            let s = self.load_state(&p, false);
            self.preview = Some(s);
        } else {
            self.preview = None;
        }
    }

    // MARK: Live refresh while scanning

    pub fn is_busy(&self) -> bool {
        self.model.scanner.is_scanning()
    }

    pub fn any_calculating(&self) -> bool {
        self.current.borrow().any_calculating
            || self.parent.as_ref().is_some_and(|p| p.borrow().any_calculating)
            || self.preview.as_ref().is_some_and(|p| p.borrow().any_calculating)
    }

    /// Call at ~12 Hz. Re-sorts the visible columns with the latest sizes when the scanner made progress.
    /// Returns true when the display needs a repaint.
    pub fn poll(&mut self) -> bool {
        let mut dirty = false;
        if let Some(f) = &self.flash {
            if Instant::now() >= f.until {
                self.flash = None;
                dirty = true;
            }
        }
        let v = self.model.scanner.version();
        if v != self.last_version {
            self.last_version = v;
            self.refresh_visible();
            dirty = true;
        }
        if dirty {
            self.touch();
        }
        dirty
    }

    pub fn refresh_visible(&mut self) {
        let (sort, rev, hid) = (self.sort, self.sort_reversed, self.show_hidden);
        let model = self.model.clone();
        {
            let filter = self.filter.clone();
            self.current.borrow_mut().rebuild(&model, sort, rev, hid, &filter);
        }
        for s in [&self.parent, &self.preview].into_iter().flatten() {
            if !Rc::ptr_eq(s, &self.current) {
                s.borrow_mut().rebuild(&model, sort, rev, hid, "");
            }
        }
        if self.pinned_to_top {
            let first = self.current.borrow().rows.first().map(|r| r.name.clone());
            if first.is_some() && first != self.cursor_name {
                self.cursor_name = first;
                self.update_preview();
            }
        }
    }

    /// The window became active again: files may have changed behind our back.
    pub fn app_activated(&mut self) {
        self.invalidate_cache();
        self.model.refresh_volumes();
        let name = self.cursor_name.clone();
        self.reload_columns();
        let valid = self.current.borrow().index_of(name.as_deref()).is_some();
        self.cursor_name = if valid { name } else { self.current.borrow().rows.first().map(|r| r.name.clone()) };
        self.update_preview();
        self.touch();
    }

    // MARK: Keyboard (yazi-style, like FileMaster)

    /// Handles a key; returns false when it is not one of ours.
    pub fn handle_key(&mut self, ev: KeyEvent) -> bool {
        if self.prompt.is_some() {
            return self.prompt_key(ev);
        }
        let page = (self.visible_rows as isize - 1).max(1);

        if let Some(prefix) = self.pending_prefix.take() {
            return match ev.key {
                Key::Char(c) if !ev.ctrl => self.handle_sequence(prefix, c),
                _ => false,
            };
        }
        if ev.alt {
            match ev.key {
                Key::Left => self.go_back(),
                Key::Right => self.go_forward(),
                Key::Up => self.leave(),
                Key::Down => self.enter(),
                _ => return false,
            }
            return true;
        }
        if ev.ctrl {
            match ev.key {
                Key::Char('d') => self.move_cursor_by(page / 2),
                Key::Char('u') => self.move_cursor_by(-(page / 2)),
                Key::Char('f') => self.start_prompt(PromptKind::Filter),
                Key::Char('b') => self.move_cursor_by(-page),
                Key::Char('a') => self.select_all(),
                Key::Char('z') => self.undo(),
                Key::Char('l') | Key::Char('g') => self.start_prompt(PromptKind::GoTo),
                Key::Char('c') => self.copy_files(),
                Key::Char('r') => self.rescan(),
                Key::Char('h') => self.toggle_hidden(),
                _ => return false,
            }
            return true;
        }
        let idx = self.cursor_index() as isize;
        match ev.key {
            Key::Up => self.move_cursor(idx - 1),
            Key::Down => self.move_cursor(idx + 1),
            Key::Left => self.leave(),
            Key::Right => self.enter(),
            Key::Enter => self.open_cursor(),
            Key::PageUp => self.move_cursor(idx - page),
            Key::PageDown => self.move_cursor(idx + page),
            Key::Home => self.move_cursor(0),
            Key::End => self.move_cursor(isize::MAX),
            Key::Escape => self.escape(),
            Key::Backspace => self.leave(),
            Key::Delete => self.request_delete(ev.shift),
            Key::Tab => self.toggle_select_and_advance(),
            Key::F5 => self.rescan(),
            Key::Char(c) => match c {
                'j' => self.move_cursor(idx + 1),
                'k' => self.move_cursor(idx - 1),
                'h' => self.leave(),
                'l' => self.enter(),
                'o' => self.open_cursor(),
                'G' => self.move_cursor(isize::MAX),
                'H' => self.go_back(),
                'L' => self.go_forward(),
                ' ' => self.open_viewer(),
                'v' => self.select_all(),
                'd' => self.request_delete(false),
                'D' => self.request_delete(true),
                '.' => self.toggle_hidden(),
                'r' => self.rescan(),
                'y' => self.copy_path(),
                'R' => self.reveal(),
                '/' | 'f' => self.start_prompt(PromptKind::Filter),
                'g' | ',' => self.pending_prefix = Some(c),
                '?' => self.events.push(Event::ShowHelp),
                _ => return false,
            },
        }
        true
    }

    fn handle_sequence(&mut self, prefix: char, key: char) -> bool {
        let h = self.model.home.clone();
        if prefix == 'g' {
            let target: Option<String> = match key {
                'g' => {
                    self.move_cursor(0);
                    return true;
                }
                'h' => Some(h),
                'd' => Some(format!("{h}\\Downloads")),
                'D' => Some(format!("{h}\\Desktop")),
                'c' => Some(format!("{h}\\Documents")),
                'a' => Some(std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into())),
                'l' => Some(format!("{h}\\AppData\\Local")),
                'T' => Some(std::env::var("TEMP").unwrap_or_else(|_| format!("{h}\\AppData\\Local\\Temp"))),
                'r' => Some(String::new()),
                't' => {
                    self.events.push(Event::OpenRecycleBin);
                    return true;
                }
                ' ' => {
                    self.start_prompt(PromptKind::GoTo);
                    return true;
                }
                _ => None,
            };
            return match target {
                Some(t) => {
                    self.cd(&t, None, true);
                    true
                }
                None => false,
            };
        }
        let rev = key.is_uppercase();
        match key.to_ascii_lowercase() {
            's' => self.set_sort(SortKey::Size, rev),
            'n' => self.set_sort(SortKey::Name, rev),
            'm' => self.set_sort(SortKey::Modified, rev),
            _ => return false,
        }
        true
    }

    fn escape(&mut self) {
        if !self.selection.is_empty() {
            self.selection.clear();
            self.touch();
        } else if !self.filter.is_empty() {
            self.apply_filter(String::new());
        }
    }

    // MARK: Prompt (filter / go to)

    pub fn start_prompt(&mut self, kind: PromptKind) {
        let text = match kind {
            PromptKind::Filter => self.filter.clone(),
            PromptKind::GoTo => {
                if self.cwd.is_empty() { String::new() } else { format!("{}\\", self.cwd.trim_end_matches('\\')) }
            }
        };
        self.prompt = Some(Prompt { kind, text });
        self.touch();
    }

    pub fn prompt_insert(&mut self, s: &str) {
        let Some(p) = &mut self.prompt else { return };
        p.text.push_str(&s.replace(['\r', '\n'], ""));
        let (kind, text) = (p.kind.clone(), p.text.clone());
        if kind == PromptKind::Filter {
            self.apply_filter(text);
        }
        self.touch();
    }

    fn prompt_key(&mut self, ev: KeyEvent) -> bool {
        let Some(p) = &mut self.prompt else { return false };
        let kind = p.kind.clone();
        match ev.key {
            Key::Escape => {
                self.prompt = None;
                if kind == PromptKind::Filter {
                    self.apply_filter(String::new());
                }
            }
            Key::Enter => {
                let text = p.text.clone();
                self.prompt = None;
                if kind == PromptKind::GoTo {
                    self.cd(&text, None, true);
                }
            }
            Key::Backspace => {
                if ev.ctrl {
                    // Delete the last word / path component.
                    let t = p.text.trim_end_matches(['\\', ' ']).to_string();
                    let cut = t.rfind(['\\', ' ']).map(|i| i + 1).unwrap_or(0);
                    p.text.truncate(cut);
                } else {
                    p.text.pop();
                }
                let text = p.text.clone();
                if kind == PromptKind::Filter {
                    self.apply_filter(text);
                }
            }
            Key::Char(c) if !ev.ctrl => {
                p.text.push(c);
                let text = p.text.clone();
                if kind == PromptKind::Filter {
                    self.apply_filter(text);
                }
            }
            Key::Char('u') if ev.ctrl => {
                p.text.clear();
                if kind == PromptKind::Filter {
                    self.apply_filter(String::new());
                }
            }
            _ => {}
        }
        self.touch();
        true
    }

    pub fn apply_filter(&mut self, s: String) {
        self.filter = s;
        let (sort, rev, hid) = (self.sort, self.sort_reversed, self.show_hidden);
        let filter = self.filter.clone();
        let model = self.model.clone();
        self.current.borrow_mut().rebuild(&model, sort, rev, hid, &filter);
        if self.current.borrow().index_of(self.cursor_name.as_deref()).is_none() {
            self.cursor_name = self.current.borrow().rows.first().map(|r| r.name.clone());
        }
        self.update_preview();
        self.touch();
    }

    // MARK: Selection

    pub fn toggle_select_and_advance(&mut self) {
        let Some(r) = self.cursor_row() else { return };
        if !self.selection.remove(&r.name) {
            self.selection.insert(r.name);
        }
        let i = self.cursor_index() as isize;
        self.move_cursor(i + 1);
    }

    pub fn select_all(&mut self) {
        self.selection = self.current.borrow().rows.iter().map(|r| r.name.clone()).collect();
        self.touch();
    }

    /// Rows an action applies to: the multi-selection, or the row under the cursor.
    pub fn targets(&self) -> Vec<Row> {
        if !self.selection.is_empty() {
            return self.current.borrow().rows.iter().filter(|r| self.selection.contains(&r.name)).cloned().collect();
        }
        self.cursor_row().into_iter().collect()
    }

    // MARK: Sorting, hidden files

    pub fn set_sort(&mut self, key: SortKey, reversed: bool) {
        self.sort = key;
        self.sort_reversed = reversed;
        self.settings.set("sort", key.id());
        self.settings.set("sortReversed", reversed);
        self.refresh_visible();
        self.set_flash(format!("Sorted by {}{}", key.title(), if reversed { " (reversed)" } else { "" }), 5);
        self.touch();
    }

    /// Menu behaviour: choosing the active key again reverses it.
    pub fn sort_by(&mut self, key: SortKey) {
        let rev = self.sort == key && !self.sort_reversed;
        self.set_sort(key, rev);
    }

    /// Menu behaviour for an explicit key choice: keeps the current reverse flag.
    pub fn sort_by_keep(&mut self, key: SortKey) {
        let rev = self.sort_reversed;
        self.set_sort(key, rev);
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.settings.set("showHidden", self.show_hidden);
        self.refresh_visible();
        if self.current.borrow().index_of(self.cursor_name.as_deref()).is_none() {
            self.cursor_name = self.current.borrow().rows.first().map(|r| r.name.clone());
        }
        self.update_preview();
        self.touch();
    }

    // MARK: Actions

    pub fn reveal(&mut self) {
        let rows = self.targets();
        if rows.is_empty() {
            let cwd = self.cwd.clone();
            self.events.push(Event::Open(cwd));
        } else {
            let ps = rows.iter().map(|r| self.path_of(&r.name)).collect();
            self.events.push(Event::Reveal(ps));
        }
    }

    pub fn copy_path(&mut self) {
        let ps: Vec<String> = self.targets().iter().map(|r| self.path_of(&r.name)).collect();
        if ps.is_empty() {
            return;
        }
        let n = ps.len();
        self.events.push(Event::CopyText(ps.join("\r\n")));
        self.set_flash(if n == 1 { "Copied path".into() } else { format!("Copied {n} paths") }, 5);
    }

    pub fn copy_files(&mut self) {
        let ps: Vec<String> = self.targets().iter().map(|r| self.path_of(&r.name)).collect();
        if !ps.is_empty() {
            let n = ps.len();
            self.events.push(Event::CopyFiles(ps));
            self.set_flash(format!("Copied {n} item{}", if n == 1 { "" } else { "s" }), 4);
        }
    }

    /// Re-scans the folder under the cursor (or the current folder) from scratch.
    pub fn rescan(&mut self) {
        self.invalidate_cache();
        let target = match self.cursor_row() {
            Some(r) if r.is_dir() && r.node.as_ref().is_some_and(|n| n.is_done() || n.state() == crate::node::State::Unlisted) => r.node,
            _ => None,
        };
        let node = match target {
            Some(n) => n,
            None if self.cwd.is_empty() => {
                self.model.refresh_volumes();
                self.reload_columns();
                self.touch();
                return;
            }
            None => self.model.node(&self.cwd),
        };
        self.model.scanner.rescan(&node);
        self.reload_columns();
        self.update_preview();
        self.touch();
    }

    pub fn set_flash(&mut self, text: String, secs: u64) {
        self.flash = Some(Flash { text, until: Instant::now() + Duration::from_secs(secs) });
        self.touch();
    }

    // MARK: Status texts

    /// "1,234 items, 56.7 GB · 3 hidden" or "2 selected · 1.2 GB".
    pub fn summary(&self) -> String {
        let s = self.current.borrow();
        if !self.selection.is_empty() {
            let (mut n, mut bytes) = (0, 0i64);
            for r in s.rows.iter().filter(|r| self.selection.contains(&r.name)) {
                n += 1;
                bytes += r.size;
            }
            return format!("{n} selected \u{b7} {}", format::bytes(bytes));
        }
        let n = s.rows.len() as i64;
        let hidden = s.entries.len() as i64 - n;
        format!(
            "{} item{}, {}{}",
            format::count(n),
            if n == 1 { "" } else { "s" },
            format::bytes(s.total),
            if self.filter.is_empty() && hidden > 0 { format!(" \u{b7} {hidden} hidden") } else { String::new() }
        )
    }

    pub fn scan_text(&self) -> Option<String> {
        let sc = &self.model.scanner;
        sc.is_scanning().then(|| format!("Scanning\u{2026} {} items", format::count(sc.items_scanned() as i64)))
    }

    // MARK: Deleting

    /// Starts a delete of the selection (or the cursor row).
    pub fn request_delete(&mut self, permanently: bool) {
        let rows = self.targets();
        if rows.is_empty() || self.deleting > 0 {
            return;
        }
        if let Some(bad) = rows.iter().find(|r| !r.safety.is_deletable()) {
            let (title, text) = if rows.len() == 1 {
                (format!("\u{201c}{}\u{201d} can't be deleted", bad.name), bad.safety.reason.to_string())
            } else {
                (
                    "Selection includes protected items".to_string(),
                    format!("\u{201c}{}\u{201d}: {}\nDeselect protected items and try again.", bad.name, bad.safety.reason),
                )
            };
            self.events.push(Event::Alert { title, text });
            return;
        }
        let items: Vec<DeleteItem> = rows
            .iter()
            .map(|r| DeleteItem {
                name: r.name.clone(),
                path: self.path_of(&r.name),
                is_dir: r.is_dir(),
                size: r.size,
                node: r.node.clone(),
            })
            .collect();
        // Final safety net: classify each path independently, in case the listing context was stale.
        for it in &items {
            let attrs = rows.iter().find(|r| r.name == it.name).map(|r| r.attrs).unwrap_or(0);
            let s = self.model.classifier.classify_path(&it.path, attrs, it.is_dir);
            if !s.is_deletable() {
                self.events.push(Event::Alert {
                    title: format!("\u{201c}{}\u{201d} can't be deleted", it.name),
                    text: s.reason.to_string(),
                });
                return;
            }
        }
        let bytes = items.iter().map(|i| i.size).sum();
        let req = DeleteRequest {
            items,
            permanently,
            dir_path: self.cwd.clone(),
            dir_node: self.current.borrow().node.clone(),
            bytes,
        };
        if permanently {
            self.events.push(Event::ConfirmDelete(req));
        } else {
            self.start_delete(req);
        }
    }

    /// Queues the (confirmed) delete for the front end to run in the background.
    pub fn start_delete(&mut self, req: DeleteRequest) {
        self.deleting += 1;
        self.set_flash(if req.permanently { "Deleting\u{2026}" } else { "Moving to the Recycle Bin\u{2026}" }.into(), 60);
        self.events.push(Event::RunDelete(req));
    }

    /// Applies a finished delete: scan bookkeeping, undo record, refresh.
    pub fn finish_delete(&mut self, req: DeleteRequest, outcome: DeleteOutcome) {
        self.deleting = self.deleting.saturating_sub(1);
        let done: HashSet<&str> = outcome.done.iter().map(|s| s.as_str()).collect();
        let mut freed = 0i64;
        let mut records = Vec::new();
        for it in &req.items {
            if !done.contains(it.path.as_str()) {
                continue;
            }
            freed += it.size;
            match (&it.node, it.is_dir) {
                (Some(n), true) => self.model.scanner.did_remove_directory(n),
                _ => self.model.scanner.did_remove_files(it.size, 1, &req.dir_node),
            }
            if !req.permanently {
                records.push(TrashRecord { original: it.path.clone(), is_dir: it.is_dir, size: it.size });
            }
        }
        if !records.is_empty() {
            self.undo_stack.push(records);
        }
        self.after_mutation(&req.dir_path);
        let n = outcome.done.len();
        if n > 0 {
            let plural = if n == 1 { "" } else { "s" };
            let text = if req.permanently {
                format!("Deleted {n} item{plural} \u{2014} freed {}", format::bytes(freed))
            } else {
                format!(
                    "Moved {n} item{plural} to the Recycle Bin ({}) \u{2014} empty it to free the space. Ctrl+Z to undo",
                    format::bytes(freed)
                )
            };
            self.set_flash(text, 8);
        } else {
            self.flash = None;
        }
        let failed: Vec<&(String, String)> = outcome.failed.iter().filter(|(_, m)| m != "Cancelled").collect();
        if !failed.is_empty() {
            let plural = if failed.len() == 1 { "" } else { "s" };
            self.events.push(Event::Alert {
                title: format!("{} item{plural} couldn't be deleted", failed.len()),
                text: failed
                    .iter()
                    .take(4)
                    .map(|(p, m)| format!("{}: {m}", paths::file_name(p)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            });
        }
    }

    /// Runs a whole delete on the calling thread (tests and scripting).
    pub fn delete_sync(&mut self, req: DeleteRequest) {
        self.deleting += 1;
        let outcome = run_delete(&req, 0);
        self.finish_delete(req, outcome);
    }

    fn after_mutation(&mut self, dir_path: &str) {
        self.selection.clear();
        self.invalidate_cache();
        self.model.refresh_volumes();
        if self.cwd != dir_path {
            return;
        }
        let idx = self.cursor_index();
        self.reload_columns();
        let rows = self.current.borrow();
        self.cursor_name = if rows.rows.is_empty() { None } else { Some(rows.rows[idx.min(rows.rows.len() - 1)].name.clone()) };
        drop(rows);
        self.update_preview();
        self.touch();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn undo(&mut self) {
        let Some(records) = self.undo_stack.pop() else { return };
        let mut restored = 0;
        for r in &records {
            match fsops::restore_from_recycle_bin(&r.original) {
                Ok(()) => {
                    restored += 1;
                    let parent = paths::parent(&r.original).unwrap_or_default();
                    if r.is_dir {
                        self.model.scan(&r.original);
                    } else {
                        let node = self.model.node(&parent);
                        self.model.scanner.did_remove_files(-r.size, -1, &node);
                    }
                }
                Err(e) => {
                    self.set_flash(format!("Couldn't restore \u{201c}{}\u{201d}: {e}", paths::file_name(&r.original)), 6);
                }
            }
        }
        let cwd = self.cwd.clone();
        self.after_mutation(&cwd);
        if restored > 0 {
            self.set_flash(format!("Restored {restored} item{}", if restored == 1 { "" } else { "s" }), 5);
        }
    }
}

/// Deletes the request's items (thread-safe; no controller state touched).
pub fn run_delete(req: &DeleteRequest, owner: isize) -> DeleteOutcome {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for it in &req.items {
        let r = if req.permanently {
            fsops::delete_permanent(&it.path).map_err(|e| e.to_string())
        } else {
            fsops::trash(&it.path, owner)
        };
        match r {
            Ok(()) => done.push(it.path.clone()),
            Err(e) => failed.push((it.path.clone(), e)),
        }
    }
    DeleteOutcome { done, failed }
}
