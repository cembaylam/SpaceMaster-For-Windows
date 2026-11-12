//! Small, dependency-free syntax tokenizer for the code/text preview.
//!
//! Covers C-like languages, scripting languages, JSON, XML/HTML/XAML (with embedded script and
//! style), CSS, key/value formats (yaml, ini, toml, properties) and Markdown. Every language is a
//! single linear pass over the bytes of the text with no per-character allocation. The returned
//! spans are sorted, contiguous and cover exactly `[0, text.len())`; every boundary lies on a
//! UTF-8 character boundary.

use std::sync::OnceLock;

// ------------------------------------------------------------------ public types

/// A language group. Several file types share one group (for example JS and TS).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Lang {
    CSharp,
    JavaScript,
    Java,
    Cpp,
    Go,
    Rust,
    Python,
    Shell,
    PowerShell,
    Sql,
    Batch,
    Json,
    Xml,
    Html,
    Css,
    KeyValue,
    Markdown,
}

impl Lang {
    /// Every language group, for iteration.
    pub const ALL: [Lang; 17] = [
        Lang::CSharp,
        Lang::JavaScript,
        Lang::Java,
        Lang::Cpp,
        Lang::Go,
        Lang::Rust,
        Lang::Python,
        Lang::Shell,
        Lang::PowerShell,
        Lang::Sql,
        Lang::Batch,
        Lang::Json,
        Lang::Xml,
        Lang::Html,
        Lang::Css,
        Lang::KeyValue,
        Lang::Markdown,
    ];
}

/// The kind of a run of text.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Tok {
    Plain,
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Tag,
    Attr,
    AttrValue,
    Preproc,
    Heading,
}

/// A run of text of one kind (byte offsets into the UTF-8 text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
    pub kind: Tok,
}

/// The VS Code "Light+" / "Dark+" colour (0xRRGGBB) for a token kind. `Plain` returns the editor's
/// default text colour (callers normally keep their own foreground for plain text).
pub fn color(kind: Tok, dark: bool) -> u32 {
    let (light, dk) = match kind {
        Tok::Plain => (0x000000, 0xD4D4D4),
        Tok::Comment => (0x008000, 0x6A9955),
        Tok::String => (0xA31515, 0xCE9178),
        Tok::Number => (0x098658, 0xB5CEA8),
        Tok::Keyword => (0x0000FF, 0x569CD6),
        Tok::Type => (0x267F99, 0x4EC9B0),
        Tok::Tag => (0x800000, 0x569CD6),
        Tok::Attr => (0xE50000, 0x9CDCFE),
        Tok::AttrValue => (0x0000FF, 0xCE9178),
        Tok::Preproc => (0xAF00DB, 0xC586C0),
        Tok::Heading => (0x0000FF, 0x569CD6),
    };
    if dark {
        dk
    } else {
        light
    }
}

/// Whether a token kind is drawn semi-bold (headings only).
pub fn is_bold(kind: Tok) -> bool {
    kind == Tok::Heading
}

// ------------------------------------------------------------------ file type mapping

/// The language for a file name (by extension / well-known names, case-insensitive), or `None`
/// when the file should not be coloured.
pub fn language_for(file_name: &str) -> Option<Lang> {
    let name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let dot = name.rfind('.')?;
    let ext = &name[dot + 1..];
    if ext.is_empty() || ext.len() > 16 || !ext.is_ascii() {
        return None;
    }
    let mut buf = [0u8; 16];
    for (d, s) in buf.iter_mut().zip(ext.bytes()) {
        *d = s.to_ascii_lowercase();
    }
    let ext = std::str::from_utf8(&buf[..ext.len()]).ok()?;
    Some(match ext {
        "cs" | "csx" => Lang::CSharp,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" => Lang::JavaScript,
        "java" | "kt" | "kts" => Lang::Java,
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh" => Lang::Cpp,
        "go" => Lang::Go,
        "rs" => Lang::Rust,
        "py" | "pyw" => Lang::Python,
        "sh" | "bash" | "zsh" => Lang::Shell,
        "ps1" | "psm1" | "psd1" => Lang::PowerShell,
        "sql" => Lang::Sql,
        "bat" | "cmd" => Lang::Batch,
        "json" | "jsonc" => Lang::Json,
        "xml" | "xaml" | "csproj" | "vbproj" | "fsproj" | "props" | "targets" | "config" | "svg"
        | "resx" | "nuspec" | "manifest" | "wxs" | "xsd" | "xslt" | "plist" | "vsixmanifest"
        | "slnx" => Lang::Xml,
        "html" | "htm" | "xhtml" => Lang::Html,
        "css" | "scss" | "less" => Lang::Css,
        "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "properties" | "editorconfig" => {
            Lang::KeyValue
        }
        "md" | "markdown" => Lang::Markdown,
        _ => return None,
    })
}

// ------------------------------------------------------------------ language table

struct Cfg {
    line: &'static [&'static [u8]],
    block: &'static [(&'static [u8], &'static [u8])],
    quotes: &'static [u8],
    backtick: bool,
    triple: bool,
    verbatim: bool,
    preproc: bool,
    dollar: bool,
    pascal: bool,
    ignore_case: bool,
    json_keys: bool,
    /// Sorted, deduplicated; lower-case when `ignore_case`.
    kw: Vec<&'static str>,
}

const C_BLOCK: &[(&[u8], &[u8])] = &[(b"/*", b"*/")];
const SLASHES: &[&[u8]] = &[b"//"];
const HASH: &[&[u8]] = &[b"#"];

fn words(s: &'static str) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = s.split_whitespace().collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn base() -> Cfg {
    Cfg {
        line: &[],
        block: &[],
        quotes: b"\"'",
        backtick: false,
        triple: false,
        verbatim: false,
        preproc: false,
        dollar: false,
        pascal: false,
        ignore_case: false,
        json_keys: false,
        kw: Vec::new(),
    }
}

fn build_cfgs() -> Vec<Cfg> {
    // Order must match the first twelve variants of `Lang`.
    vec![
        Cfg {
            line: SLASHES, block: C_BLOCK, verbatim: true, preproc: true, pascal: true,
            kw: words("abstract as base bool break byte case catch char checked class const continue decimal default delegate do double else enum event explicit extern false finally fixed float for foreach goto if implicit in int interface internal is lock long namespace new null object operator out override params private protected public readonly ref return sbyte sealed short sizeof stackalloc static string struct switch this throw true try typeof uint ulong unchecked unsafe ushort using virtual void volatile while var async await record init required partial get set value when yield nameof dynamic global with and or not"),
            ..base()
        },
        Cfg {
            line: SLASHES, block: C_BLOCK, backtick: true, pascal: true,
            kw: words("break case catch class const continue debugger default delete do else export extends finally for function if import in instanceof let new return super switch this throw try typeof var void while with yield async await of static get set true false null undefined interface type enum implements private public protected readonly abstract as from namespace declare number string boolean any"),
            ..base()
        },
        Cfg {
            line: SLASHES, block: C_BLOCK, pascal: true,
            kw: words("abstract assert boolean break byte case catch char class const continue default do double else enum extends final finally float for goto if implements import instanceof int interface long native new null package private protected public return short static strictfp super switch synchronized this throw throws transient true false try void volatile while var val fun when object companion data sealed override lateinit"),
            ..base()
        },
        Cfg {
            line: SLASHES, block: C_BLOCK, preproc: true,
            kw: words("auto break case char const continue default do double else enum extern float for goto if inline int long register return short signed sizeof static struct switch typedef union unsigned void volatile while class namespace template typename public private protected virtual new delete this using nullptr true false bool try catch throw constexpr"),
            ..base()
        },
        Cfg {
            line: SLASHES, block: C_BLOCK, backtick: true,
            kw: words("break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var true false nil"),
            ..base()
        },
        Cfg {
            // ' is a lifetime marker, not a string
            line: SLASHES, block: C_BLOCK, quotes: b"\"", pascal: true,
            kw: words("as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while"),
            ..base()
        },
        Cfg {
            line: HASH, triple: true,
            kw: words("and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield self"),
            ..base()
        },
        Cfg {
            line: HASH, quotes: b"\"'`", dollar: true,
            kw: words("if then else elif fi for while until do done case esac function in select time echo export local return exit set unset source alias cd test"),
            ..base()
        },
        Cfg {
            line: HASH, block: &[(b"<#", b"#>")], dollar: true, ignore_case: true,
            kw: words("function filter param begin process end if elseif else switch foreach for while do until break continue return throw try catch finally in class enum using"),
            ..base()
        },
        Cfg {
            line: &[b"--"], block: C_BLOCK, quotes: b"'\"", ignore_case: true,
            kw: words("select from where group by order having insert into values update set delete create alter drop table index view join inner left right outer full on as and or not null is in like between exists union all distinct limit offset case when then else end primary key foreign references default constraint unique check begin commit rollback declare exec execute procedure function trigger top asc desc count sum avg min max cast convert int varchar nvarchar bit datetime decimal"),
            ..base()
        },
        Cfg {
            line: &[b"::", b"REM ", b"rem "], quotes: b"\"", ignore_case: true,
            kw: words("if else goto echo set call for in do exit not exist errorlevel setlocal endlocal pause start cd copy del dir move md rd type"),
            ..base()
        },
        Cfg {
            line: SLASHES, block: C_BLOCK, quotes: b"\"", json_keys: true,
            kw: words("true false null"),
            ..base()
        },
    ]
}

fn cfg(l: Lang) -> &'static Cfg {
    static CFGS: OnceLock<Vec<Cfg>> = OnceLock::new();
    &CFGS.get_or_init(build_cfgs)[l as usize]
}

// ------------------------------------------------------------------ entry point

/// Tokenizes `text` in one linear pass. The spans are sorted, contiguous and cover `[0, text.len())`.
pub fn tokenize(text: &str, lang: Lang) -> Vec<Span> {
    let mut o = Out { v: Vec::with_capacity((text.len() / 6).max(16)) };
    let len = text.len();
    match lang {
        Lang::Xml => xml(text, 0, len, false, &mut o),
        Lang::Html => xml(text, 0, len, true, &mut o),
        Lang::Css => css(text, 0, len, &mut o),
        Lang::KeyValue => key_value(text, &mut o),
        Lang::Markdown => markdown(text, &mut o),
        _ => generic(text, 0, len, cfg(lang), &mut o),
    }
    debug_assert!(o.v.last().map_or(len == 0, |s| s.start + s.len == len));
    o.v
}

struct Out {
    v: Vec<Span>,
}

impl Out {
    #[inline]
    fn emit(&mut self, start: usize, end: usize, kind: Tok) {
        if end <= start {
            return;
        }
        if let Some(l) = self.v.last_mut() {
            if l.kind == kind && l.start + l.len == start {
                l.len += end - start;
                return;
            }
        }
        self.v.push(Span { start, len: end - start, kind });
    }
}

// ------------------------------------------------------------------ helpers

#[inline]
fn char_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >= 0xF0 {
        4
    } else if first >= 0xE0 {
        3
    } else {
        2
    }
}

#[inline]
fn char_at(t: &str, i: usize) -> char {
    t[i..].chars().next().unwrap_or('\0')
}

/// Length of the whitespace character at `i`, or 0.
#[inline]
fn ws_len(t: &str, i: usize) -> usize {
    let c = t.as_bytes()[i];
    if c < 0x80 {
        matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) as usize
    } else if char_at(t, i).is_whitespace() {
        char_len(c)
    } else {
        0
    }
}

/// Length of the letter at `i`, or 0.
#[inline]
fn letter_len(t: &str, i: usize) -> usize {
    let c = t.as_bytes()[i];
    if c < 0x80 {
        c.is_ascii_alphabetic() as usize
    } else if char_at(t, i).is_alphabetic() {
        char_len(c)
    } else {
        0
    }
}

/// Length of the letter-or-digit at `i`, or 0.
#[inline]
fn alnum_len(t: &str, i: usize) -> usize {
    let c = t.as_bytes()[i];
    if c < 0x80 {
        c.is_ascii_alphanumeric() as usize
    } else if char_at(t, i).is_alphanumeric() {
        char_len(c)
    } else {
        0
    }
}

#[inline]
fn snap(t: &str, j: usize, end: usize) -> usize {
    let mut j = j.min(end);
    while j < end && !t.is_char_boundary(j) {
        j += 1;
    }
    j
}

#[inline]
fn starts(b: &[u8], i: usize, end: usize, n: &[u8]) -> bool {
    i + n.len() <= end && &b[i..i + n.len()] == n
}

#[inline]
fn find_byte(b: &[u8], from: usize, end: usize, x: u8) -> Option<usize> {
    if from >= end {
        return None;
    }
    b[from..end].iter().position(|&c| c == x).map(|p| p + from)
}

fn find(b: &[u8], from: usize, end: usize, n: &[u8]) -> Option<usize> {
    if from > end || end - from < n.len() {
        return None;
    }
    let last = end - n.len();
    let mut i = from;
    while i <= last {
        let p = b[i..=last].iter().position(|&c| c == n[0])?;
        i += p;
        if &b[i..i + n.len()] == n {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Case-insensitive (ASCII) search; `n` must be lower-case.
fn find_ci(b: &[u8], from: usize, end: usize, n: &[u8]) -> Option<usize> {
    if from > end || end - from < n.len() {
        return None;
    }
    let last = end - n.len();
    (from..=last).find(|&i| b[i..i + n.len()].iter().zip(n).all(|(a, c)| a.to_ascii_lowercase() == *c))
}

fn line_end(b: &[u8], i: usize, end: usize) -> usize {
    find_byte(b, i, end, b'\n').unwrap_or(end)
}

fn is_keyword(cfg: &Cfg, word: &[u8]) -> bool {
    if word.len() > 16 {
        return false;
    }
    let mut buf = [0u8; 16];
    let w: &[u8] = if cfg.ignore_case {
        for (d, s) in buf.iter_mut().zip(word) {
            *d = s.to_ascii_lowercase();
        }
        &buf[..word.len()]
    } else {
        word
    };
    cfg.kw.binary_search_by(|k| k.as_bytes().cmp(w)).is_ok()
}

fn is_all_upper(w: &str) -> bool {
    w.chars().all(|c| !c.is_alphabetic() || c.is_uppercase())
}

// ------------------------------------------------------------------ generic (C-like, scripts, json)

fn generic(t: &str, start: usize, end: usize, cfg: &Cfg, o: &mut Out) {
    let b = t.as_bytes();
    let mut i = start;
    let mut line_blank = true; // only whitespace so far on this line

    while i < end {
        let c = b[i];

        if c == b'\n' {
            o.emit(i, i + 1, Tok::Plain);
            i += 1;
            line_blank = true;
            continue;
        }
        let wl = ws_len(t, i);
        if wl > 0 {
            let mut j = i + wl;
            while j < end && b[j] != b'\n' {
                let w = ws_len(t, j);
                if w == 0 {
                    break;
                }
                j += w;
            }
            o.emit(i, j, Tok::Plain);
            i = j;
            continue;
        }

        // line comments
        let mut matched = false;
        for lc in cfg.line {
            if starts(b, i, end, lc) && (!lc[0].is_ascii_alphabetic() || line_blank) {
                let j = line_end(b, i, end);
                o.emit(i, j, Tok::Comment);
                i = j;
                matched = true;
                break;
            }
        }
        if matched {
            line_blank = false;
            continue;
        }

        // block comments
        for (bs, be) in cfg.block {
            if starts(b, i, end, bs) {
                let e = match find(b, i + bs.len(), end, be) {
                    Some(k) => k + be.len(),
                    None => end,
                };
                o.emit(i, e, Tok::Comment);
                i = e;
                matched = true;
                break;
            }
        }
        if matched {
            line_blank = false;
            continue;
        }

        // preprocessor directive (#if, #include, #region ...): colour the directive word only
        if cfg.preproc && c == b'#' && line_blank {
            let mut j = i + 1;
            while j < end && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            while j < end {
                let l = letter_len(t, j);
                if l == 0 {
                    break;
                }
                j += l;
            }
            o.emit(i, j, Tok::Preproc);
            i = j;
            line_blank = false;
            continue;
        }

        line_blank = false;

        // python triple-quoted strings
        if cfg.triple && (c == b'"' || c == b'\'') && i + 2 < end && b[i + 1] == c && b[i + 2] == c {
            let e = match find(b, i + 3, end, &[c, c, c]) {
                Some(k) => k + 3,
                None => end,
            };
            o.emit(i, e, Tok::String);
            i = e;
            continue;
        }

        // C# string prefixes: $"..." @"..." $@"..."
        let mut prefix = 0;
        let mut verbatim = false;
        if cfg.verbatim && (c == b'@' || c == b'$') {
            let mut p = i;
            while p < end && (b[p] == b'@' || b[p] == b'$') && p - i < 2 {
                if b[p] == b'@' {
                    verbatim = true;
                }
                p += 1;
            }
            if p < end && b[p] == b'"' {
                prefix = p - i;
            } else {
                verbatim = false;
            }
        }

        let q = if prefix > 0 { b'"' } else { c };
        let is_quote = prefix > 0 || cfg.quotes.contains(&c) || (cfg.backtick && c == b'`');
        if is_quote {
            let multiline = verbatim || q == b'`';
            let mut j = i + prefix + 1;
            while j < end {
                let ch = b[j];
                if verbatim {
                    if ch == b'"' {
                        if j + 1 < end && b[j + 1] == b'"' {
                            j += 2;
                            continue;
                        }
                        j += 1;
                        break;
                    }
                    j += 1;
                    continue;
                }
                if ch == b'\\' {
                    j += 2;
                    continue;
                }
                if ch == q {
                    j += 1;
                    break;
                }
                if ch == b'\n' && !multiline {
                    break; // unterminated on this line: stop before the newline
                }
                j += 1;
            }
            let j = snap(t, j, end);

            let mut kind = Tok::String;
            if cfg.json_keys {
                let mut n = j;
                while n < end && (b[n] == b' ' || b[n] == b'\t') {
                    n += 1;
                }
                if n < end && b[n] == b':' {
                    kind = Tok::Attr; // "key":
                }
            }
            o.emit(i, j, kind);
            i = j;
            continue;
        }

        // numbers
        if c.is_ascii_digit()
            || (c == b'.'
                && i + 1 < end
                && b[i + 1].is_ascii_digit()
                && (i == start || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] >= 0x80)))
        {
            let mut j = i + 1;
            while j < end {
                let l = alnum_len(t, j);
                if l > 0 {
                    j += l;
                } else if b[j] == b'_' || (b[j] == b'.' && j + 1 < end && b[j + 1].is_ascii_digit()) {
                    j += 1;
                } else {
                    break;
                }
            }
            o.emit(i, j, Tok::Number);
            i = j;
            continue;
        }

        // identifiers, keywords, $variables
        let ll = letter_len(t, i);
        if ll > 0 || c == b'_' || (cfg.dollar && c == b'$') {
            let mut j = i + ll.max(1);
            while j < end {
                let l = alnum_len(t, j);
                if l > 0 {
                    j += l;
                } else if b[j] == b'_' {
                    j += 1;
                } else {
                    break;
                }
            }
            let kind = if c == b'$' {
                Tok::Attr
            } else if is_keyword(cfg, &b[i..j]) {
                Tok::Keyword
            } else if cfg.pascal
                && j - i > 1
                && (c.is_ascii_uppercase() || (c >= 0x80 && char_at(t, i).is_uppercase()))
                && !is_all_upper(&t[i..j])
            {
                Tok::Type
            } else {
                Tok::Plain
            };
            o.emit(i, j, kind);
            i = j;
            continue;
        }

        // punctuation, operators
        let n = char_len(c);
        o.emit(i, i + n, Tok::Plain);
        i += n;
    }
}

// ------------------------------------------------------------------ XML / HTML / XAML

fn xml(t: &str, start: usize, end: usize, html: bool, o: &mut Out) {
    let b = t.as_bytes();
    let mut i = start;
    while i < end {
        let lt = match find_byte(b, i, end, b'<') {
            Some(p) => p,
            None => {
                xml_text(t, i, end, o);
                break;
            }
        };
        xml_text(t, i, lt, o);
        i = lt;

        if starts(b, i, end, b"<!--") {
            let e = find(b, i + 4, end, b"-->").map_or(end, |k| k + 3);
            o.emit(i, e, Tok::Comment);
            i = e;
            continue;
        }
        if starts(b, i, end, b"<![CDATA[") {
            let e = find(b, i + 9, end, b"]]>").map_or(end, |k| k + 3);
            o.emit(i, e, Tok::String);
            i = e;
            continue;
        }
        if starts(b, i, end, b"<?") {
            let e = find(b, i + 2, end, b"?>").map_or(end, |k| k + 2);
            o.emit(i, e, Tok::Preproc);
            i = e;
            continue;
        }
        if starts(b, i, end, b"<!") {
            let e = find_byte(b, i, end, b'>').map_or(end, |k| k + 1);
            o.emit(i, e, Tok::Preproc);
            i = e;
            continue;
        }

        // a tag: <name ...> or </name>
        let mut j = i + 1;
        let closing = j < end && b[j] == b'/';
        if closing {
            j += 1;
        }
        let name_start = j;
        while j < end {
            let l = alnum_len(t, j);
            if l > 0 {
                j += l;
            } else if matches!(b[j], b':' | b'-' | b'_' | b'.') {
                j += 1;
            } else {
                break;
            }
        }
        if j == name_start {
            o.emit(i, i + 1, Tok::Plain); // a lone '<'
            i += 1;
            continue;
        }
        let name = &b[name_start..j];
        o.emit(i, j, Tok::Tag);

        while j < end && b[j] != b'>' {
            let c = b[j];
            let wl = ws_len(t, j);
            if wl > 0 {
                let mut w = j + wl;
                while w < end {
                    let l = ws_len(t, w);
                    if l == 0 {
                        break;
                    }
                    w += l;
                }
                o.emit(j, w, Tok::Plain);
                j = w;
                continue;
            }
            if c == b'/' {
                o.emit(j, j + 1, Tok::Tag);
                j += 1;
                continue;
            }
            if c == b'=' {
                o.emit(j, j + 1, Tok::Plain);
                j += 1;
                continue;
            }
            if c == b'"' || c == b'\'' {
                let e = find_byte(b, j + 1, end, c).map_or(end, |k| k + 1);
                o.emit(j, e, Tok::AttrValue);
                j = e;
                continue;
            }
            let mut a = j;
            while a < end
                && !matches!(b[a], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C | b'=' | b'>' | b'/' | b'"' | b'\'')
            {
                a += 1;
            }
            if a == j {
                a = j + 1;
            }
            o.emit(j, a, Tok::Attr);
            j = a;
        }
        if j < end {
            o.emit(j, j + 1, Tok::Tag); // '>'
            j += 1;
        }
        i = j;

        // <script> and <style> bodies in HTML are code
        if html && !closing && (i < 2 || b[i - 2] != b'/') {
            let script = name.eq_ignore_ascii_case(b"script");
            let style = name.eq_ignore_ascii_case(b"style");
            if script || style {
                let mut needle = [0u8; 8];
                needle[0] = b'<';
                needle[1] = b'/';
                for (d, s) in needle[2..].iter_mut().zip(name) {
                    *d = s.to_ascii_lowercase();
                }
                let close = find_ci(b, i, end, &needle[..2 + name.len()]);
                let body_end = close.unwrap_or(end);
                if script {
                    generic(t, i, body_end, cfg_js(), o);
                } else {
                    css(t, i, body_end, o);
                }
                i = body_end;
            }
        }
    }
}

fn cfg_js() -> &'static Cfg {
    cfg(Lang::JavaScript)
}

/// Text between tags: plain, with `&entities;` picked out.
fn xml_text(t: &str, start: usize, end: usize, o: &mut Out) {
    let b = t.as_bytes();
    let mut i = start;
    while i < end {
        let amp = match find_byte(b, i, end, b'&') {
            Some(p) => p,
            None => {
                o.emit(i, end, Tok::Plain);
                return;
            }
        };
        o.emit(i, amp, Tok::Plain);
        let mut j = amp + 1;
        while j < end && j - amp < 10 && (b[j].is_ascii_alphanumeric() || b[j] == b'#') {
            j += 1;
        }
        if j < end && b[j] == b';' && j > amp + 1 {
            o.emit(amp, j + 1, Tok::Number);
            i = j + 1;
        } else {
            o.emit(amp, amp + 1, Tok::Plain);
            i = amp + 1;
        }
    }
}

// ------------------------------------------------------------------ CSS

fn css(t: &str, start: usize, end: usize, o: &mut Out) {
    let b = t.as_bytes();
    let mut i = start;
    let mut depth = 0usize;
    while i < end {
        let c = b[i];
        let wl = ws_len(t, i);
        if wl > 0 {
            let mut j = i + wl;
            while j < end {
                let l = ws_len(t, j);
                if l == 0 {
                    break;
                }
                j += l;
            }
            o.emit(i, j, Tok::Plain);
            i = j;
            continue;
        }

        if c == b'/' && i + 1 < end && b[i + 1] == b'*' {
            let e = find(b, i + 2, end, b"*/").map_or(end, |k| k + 2);
            o.emit(i, e, Tok::Comment);
            i = e;
            continue;
        }

        if c == b'"' || c == b'\'' {
            let mut j = i + 1;
            while j < end && b[j] != c && b[j] != b'\n' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            let j = snap(t, j + 1, end);
            o.emit(i, j, Tok::String);
            i = j;
            continue;
        }

        if c == b'{' {
            depth += 1;
            o.emit(i, i + 1, Tok::Plain);
            i += 1;
            continue;
        }
        if c == b'}' {
            depth = depth.saturating_sub(1);
            o.emit(i, i + 1, Tok::Plain);
            i += 1;
            continue;
        }

        if c == b'@' {
            let mut j = i + 1;
            while j < end {
                let l = letter_len(t, j);
                if l > 0 {
                    j += l;
                } else if b[j] == b'-' {
                    j += 1;
                } else {
                    break;
                }
            }
            o.emit(i, j, Tok::Keyword);
            i = j;
            continue;
        }

        if c == b'#' && depth > 0 {
            let mut j = i + 1;
            while j < end && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            o.emit(i, j, if j > i + 1 { Tok::Number } else { Tok::Plain });
            i = j.max(i + 1);
            continue;
        }

        if c.is_ascii_digit() || (c == b'.' && depth > 0 && i + 1 < end && b[i + 1].is_ascii_digit()) {
            let mut j = i + 1;
            while j < end {
                let l = alnum_len(t, j);
                if l > 0 {
                    j += l;
                } else if b[j] == b'.' || b[j] == b'%' {
                    j += 1;
                } else {
                    break;
                }
            }
            o.emit(i, j, Tok::Number);
            i = j;
            continue;
        }

        let ll = letter_len(t, i);
        if ll > 0
            || c == b'_'
            || c == b'-'
            || ((c == b'.' || c == b'#') && depth == 0 && i + 1 < end && letter_len(t, i + 1) > 0)
        {
            let mut j = i + ll.max(1);
            while j < end {
                let l = alnum_len(t, j);
                if l > 0 {
                    j += l;
                } else if b[j] == b'_' || b[j] == b'-' {
                    j += 1;
                } else {
                    break;
                }
            }
            let mut n = j;
            while n < end && (b[n] == b' ' || b[n] == b'\t') {
                n += 1;
            }
            let kind = if depth == 0 {
                Tok::Type
            } else if n < end && b[n] == b':' {
                Tok::Attr
            } else {
                Tok::Plain
            };
            o.emit(i, j, kind);
            i = j;
            continue;
        }

        let n = char_len(c);
        o.emit(i, i + n, Tok::Plain);
        i += n;
    }
}

// ------------------------------------------------------------------ key/value (yaml, ini, toml, properties)

fn key_value(t: &str, o: &mut Out) {
    let b = t.as_bytes();
    let len = b.len();
    let mut i = 0;
    while i < len {
        let e = line_end(b, i, len);
        let mut s = i;
        while s < e && (b[s] == b' ' || b[s] == b'\t') {
            s += 1;
        }
        o.emit(i, s, Tok::Plain);

        let rb = if s < e && b[s] == b'[' { find_byte(b, s, e, b']') } else { None };
        if s < e && (b[s] == b'#' || b[s] == b';') {
            o.emit(s, e, Tok::Comment);
        } else if let Some(rb) = rb {
            o.emit(s, rb + 1, Tok::Type);
            o.emit(rb + 1, e, Tok::Plain);
        } else {
            let mut p = s;
            if p + 1 < e && b[p] == b'-' && b[p + 1] == b' ' {
                o.emit(p, p + 1, Tok::Keyword);
                p += 1;
            }
            let mut sep = None;
            let mut quoted = false;
            let mut qc = 0u8;
            for k in p..e {
                let ch = b[k];
                if quoted {
                    if ch == qc {
                        quoted = false;
                    }
                    continue;
                }
                if ch == b'"' || ch == b'\'' {
                    quoted = true;
                    qc = ch;
                    continue;
                }
                if (ch == b'=' || ch == b':') && (ch == b'=' || k + 1 >= e || b[k + 1] == b' ') {
                    sep = Some(k);
                    break;
                }
            }
            match sep {
                Some(sep) if sep > p => {
                    let mut ke = sep;
                    while ke > p && (b[ke - 1] == b' ' || b[ke - 1] == b'\t') {
                        ke -= 1;
                    }
                    o.emit(p, ke, Tok::Attr);
                    o.emit(ke, sep + 1, Tok::Plain);
                    key_value_value(t, sep + 1, e, o);
                }
                _ => key_value_value(t, p, e, o),
            }
        }

        if e < len {
            o.emit(e, e + 1, Tok::Plain);
        }
        i = e + 1;
    }
}

fn is_number_text(v: &str) -> bool {
    let rest = v.strip_prefix(['+', '-']).unwrap_or(v);
    match rest.as_bytes().first() {
        Some(c) if c.is_ascii_digit() || *c == b'.' => v.parse::<f64>().is_ok(),
        _ => false,
    }
}

fn key_value_value(t: &str, start: usize, end: usize, o: &mut Out) {
    let b = t.as_bytes();
    let mut s = start;
    while s < end && (b[s] == b' ' || b[s] == b'\t') {
        s += 1;
    }
    o.emit(start, s, Tok::Plain);
    if s >= end {
        return;
    }

    // trailing " # comment"
    let mut hash = None;
    let mut quoted = false;
    let mut qc = 0u8;
    for k in s..end {
        let ch = b[k];
        if quoted {
            if ch == qc {
                quoted = false;
            }
            continue;
        }
        if ch == b'"' || ch == b'\'' {
            quoted = true;
            qc = ch;
            continue;
        }
        if ch == b'#' && (k == s || b[k - 1] == b' ') {
            hash = Some(k);
            break;
        }
    }

    let ve = hash.unwrap_or(end);
    let mut te = ve;
    while te > s && (b[te - 1] == b' ' || b[te - 1] == b'\t') {
        te -= 1;
    }
    let v = &t[s..te];
    let vb = v.as_bytes();

    let kind = if !vb.is_empty() && (vb[0] == b'"' || vb[0] == b'\'') {
        Tok::String
    } else if ["true", "false", "null", "yes", "no", "on", "off"].iter().any(|k| v.eq_ignore_ascii_case(k)) {
        Tok::Keyword
    } else if is_number_text(v) {
        Tok::Number
    } else {
        Tok::Plain
    };

    o.emit(s, te, kind);
    o.emit(te, ve, Tok::Plain);
    if let Some(h) = hash {
        o.emit(h, end, Tok::Comment);
    }
}

// ------------------------------------------------------------------ Markdown

fn markdown(t: &str, o: &mut Out) {
    let b = t.as_bytes();
    let len = b.len();
    let mut i = 0;
    let mut fenced = false;
    while i < len {
        let e = line_end(b, i, len);
        let mut s = i;
        while s < e && b[s] == b' ' {
            s += 1;
        }

        if starts(b, s, len, b"```") || starts(b, s, len, b"~~~") {
            fenced = !fenced;
            o.emit(i, e, Tok::Comment);
        } else if fenced {
            o.emit(i, e, Tok::String);
        } else if s < e && b[s] == b'#' {
            o.emit(i, e, Tok::Heading);
        } else if s < e && b[s] == b'>' {
            o.emit(i, e, Tok::Comment);
        } else {
            let mut p = s;
            // list marker: "- ", "* ", "+ " or "1. "
            if p + 1 < e && matches!(b[p], b'-' | b'*' | b'+') && b[p + 1] == b' ' {
                o.emit(i, p, Tok::Plain);
                o.emit(p, p + 1, Tok::Keyword);
                p += 1;
            } else {
                let mut d = p;
                while d < e && b[d].is_ascii_digit() {
                    d += 1;
                }
                if d > p && d + 1 < e && b[d] == b'.' && b[d + 1] == b' ' {
                    o.emit(i, p, Tok::Plain);
                    o.emit(p, d + 1, Tok::Keyword);
                    p = d + 1;
                } else {
                    p = i;
                }
            }

            // inline `code`
            let mut k = p;
            while k < e {
                let Some(b1) = find_byte(b, k, e, b'`') else {
                    o.emit(k, e, Tok::Plain);
                    break;
                };
                let Some(b2) = find_byte(b, b1 + 1, e, b'`') else {
                    o.emit(k, e, Tok::Plain);
                    break;
                };
                o.emit(k, b1, Tok::Plain);
                o.emit(b1, b2 + 1, Tok::String);
                k = b2 + 1;
            }
        }

        if e < len {
            o.emit(e, e + 1, Tok::Plain);
        }
        i = e + 1;
    }
}
