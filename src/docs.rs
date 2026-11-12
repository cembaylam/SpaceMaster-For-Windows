//! Document previews: file kind classification, Office/OpenDocument text extraction, archive listings,
//! hex dumps, bounded text/byte head reads and Markdown rendering.
//!
//! Everything here is UI independent and bounded: files are never read whole (only a head, or a streamed
//! prefix), archives are listed from their directory structures wherever the format allows it.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, Write};
use std::path::Path;

use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::Reader;

// ====================================================================================================
// File kinds
// ====================================================================================================

/// Coarse file classification by extension.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Image,
    Pdf,
    Video,
    Audio,
    Text,
    Office,
    Archive,
    Other,
}

/// Extension (including the leading dot) of a file name, like .NET `Path.GetExtension`
/// (so `.gitignore` has the extension `.gitignore`). Empty when there is none.
fn extension_of(name: &str) -> &str {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rfind('.') {
        Some(i) if i + 1 < base.len() => &base[i..],
        _ => "",
    }
}

const IMAGES: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".tif", ".tiff", ".ico", ".jxr", ".heic", ".avif",
];
const VIDEOS: &[&str] = &[
    ".mp4", ".mkv", ".avi", ".mov", ".wmv", ".webm", ".m4v", ".flv", ".mpg", ".mpeg", ".3gp", ".mts", ".m2ts",
];
const AUDIOS: &[&str] = &[".mp3", ".wav", ".flac", ".m4a", ".aac", ".ogg", ".wma", ".opus"];
const ARCHIVES: &[&str] = &[
    ".zip", ".7z", ".rar", ".tar", ".gz", ".tgz", ".bz2", ".tbz2", ".xz", ".txz", ".cab", ".iso", ".jar", ".apk",
    ".nupkg", ".vsix", ".whl",
];
const OFFICES: &[&str] = &[
    ".docx", ".docm", ".dotx", ".xlsx", ".xlsm", ".xltx", ".pptx", ".pptm", ".potx", ".ppsx", ".doc", ".xls", ".ppt",
    ".odt", ".ods", ".odp",
];
const TEXTS: &[&str] = &[
    ".txt", ".md", ".json", ".xml", ".yaml", ".yml", ".toml", ".ini", ".cfg", ".conf", ".log", ".csv", ".tsv", ".cs",
    ".rs", ".py", ".js", ".ts", ".tsx", ".jsx", ".html", ".htm", ".css", ".xaml", ".csproj", ".sln", ".c", ".h",
    ".cpp", ".hpp", ".go", ".java", ".kt", ".sh", ".ps1", ".bat", ".cmd", ".sql", ".lua", ".rb", ".php", ".tex",
    ".rst", ".gitignore", ".props", ".targets", ".svg", ".reg", ".vue", ".swift",
];

/// Classifies a file name by its extension (case-insensitive). Allocation free.
pub fn kind_of(file_name: &str) -> Kind {
    let ext = extension_of(file_name);
    if ext.is_empty() || ext.len() > 12 {
        return Kind::Other;
    }
    let has = |set: &[&str]| set.iter().any(|e| e.eq_ignore_ascii_case(ext));
    if has(IMAGES) {
        Kind::Image
    } else if has(VIDEOS) {
        Kind::Video
    } else if has(AUDIOS) {
        Kind::Audio
    } else if ext.eq_ignore_ascii_case(".pdf") {
        Kind::Pdf
    } else if has(ARCHIVES) {
        Kind::Archive
    } else if has(OFFICES) {
        Kind::Office
    } else if has(TEXTS) {
        Kind::Text
    } else {
        Kind::Other
    }
}

/// Files the preview renders as a page (HTML as is, Markdown converted).
pub fn is_renderable(file_name: &str) -> bool {
    let ext = extension_of(file_name);
    [".html", ".htm", ".md", ".markdown"].iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// A text preview plus a one-line caption.
pub struct TextDoc {
    pub text: String,
    pub caption: String,
}

// ====================================================================================================
// Hex dump
// ====================================================================================================

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Hex-editor style dump: offset, 16 bytes in hex (8 + 8), decoded text.
pub fn hex_dump(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(empty file)".to_string();
    }
    const PER_ROW: usize = 16;
    let rows = bytes.len().div_ceil(PER_ROW);
    let mut out: Vec<u8> = Vec::with_capacity(rows * (PER_ROW * 4 + 14) + 120);

    out.extend_from_slice(b"Offset    ");
    for j in 0..PER_ROW {
        out.push(HEX[(j >> 4) & 15]);
        out.push(HEX[j & 15]);
        out.push(b' ');
        if j == 7 {
            out.push(b' ');
        }
    }
    out.extend_from_slice(b"  Decoded text\n");
    out.extend_from_slice(b"--------  ");
    for j in 0..PER_ROW {
        out.extend_from_slice(b"-- ");
        if j == 7 {
            out.push(b' ');
        }
    }
    out.extend_from_slice(b"  ------------\n");

    for (r, chunk) in bytes.chunks(PER_ROW).enumerate() {
        let off = (r * PER_ROW) as u64;
        for shift in (0..8).rev() {
            out.push(HEX[((off >> (shift * 4)) & 15) as usize]);
        }
        out.extend_from_slice(b"  ");
        for j in 0..PER_ROW {
            if let Some(&b) = chunk.get(j) {
                out.push(HEX[(b >> 4) as usize]);
                out.push(HEX[(b & 15) as usize]);
                out.push(b' ');
            } else {
                out.extend_from_slice(b"   ");
            }
            if j == 7 {
                out.push(b' ');
            }
        }
        out.extend_from_slice(b"  ");
        out.extend(chunk.iter().map(|&b| if (0x20..=0x7E).contains(&b) { b } else { b'.' }));
        out.push(b'\n');
    }
    // Only ASCII was written.
    String::from_utf8(out).unwrap_or_default()
}

// ====================================================================================================
// Bounded head reads
// ====================================================================================================

fn read_head(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let f = File::open(path)?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let want = len.min(max_bytes as u64) as usize;
    let mut buf = Vec::with_capacity(want.max(1));
    f.take(max_bytes as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// First `max_bytes` bytes of a file (for the hex view).
pub fn read_head_bytes(path: &Path, max_bytes: usize) -> Option<Vec<u8>> {
    read_head(path, max_bytes).ok()
}

const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{2C6}', '\u{2030}',
    '\u{160}', '\u{2039}', '\u{152}', '\u{8D}', '\u{17D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}',
    '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{2DC}', '\u{2122}', '\u{161}', '\u{203A}', '\u{153}', '\u{9D}',
    '\u{17E}', '\u{178}',
];

fn decode_cp1252(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() + b.len() / 8);
    for &c in b {
        s.push(match c {
            0x80..=0x9F => CP1252_HIGH[(c - 0x80) as usize],
            _ => c as char,
        });
    }
    s
}

fn decode_utf16(b: &[u8], big_endian: bool) -> String {
    let mut units: Vec<u16> = b
        .chunks_exact(2)
        .map(|p| if big_endian { u16::from_be_bytes([p[0], p[1]]) } else { u16::from_le_bytes([p[0], p[1]]) })
        .collect();
    // A trailing lone high surrogate means the head was cut in the middle of a character: drop it.
    if units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
        units.pop();
    }
    char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect()
}

/// Tabs become 4 spaces and CRLF becomes LF, in one pass.
fn normalize_text(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 16);
    let mut start = 0;
    for i in 0..b.len() {
        match b[i] {
            b'\t' => {
                out.push_str(&s[start..i]);
                out.push_str("    ");
                start = i + 1;
            }
            b'\r' if b.get(i + 1) == Some(&b'\n') => {
                out.push_str(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push_str(&s[start..]);
    out
}

fn decode_utf8_or_1252(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        // Incomplete sequence at the very end: the head was cut mid-character, keep the valid part.
        Err(e) if e.error_len().is_none() => String::from_utf8_lossy(&b[..e.valid_up_to()]).into_owned(),
        Err(_) => decode_cp1252(b),
    }
}

/// Reads up to `max_bytes` as text. `None` when the file looks binary (a NUL byte in the first 8 KiB without a
/// UTF-16 BOM) or cannot be read. Handles UTF-8 / UTF-16 BOMs, falls back to Windows-1252 for invalid UTF-8,
/// and never splits a character at the cut. Tabs become 4 spaces, CRLF becomes LF.
pub fn read_text_head(path: &Path, max_bytes: usize) -> Option<String> {
    let buf = read_head(path, max_bytes).ok()?;
    let n = buf.len();
    if n == 0 {
        return Some(String::new());
    }
    let decoded = if n >= 3 && buf[..3] == [0xEF, 0xBB, 0xBF] {
        decode_utf8_or_1252(&buf[3..])
    } else if n >= 2 && buf[..2] == [0xFF, 0xFE] {
        decode_utf16(&buf[2..], false)
    } else if n >= 2 && buf[..2] == [0xFE, 0xFF] {
        decode_utf16(&buf[2..], true)
    } else {
        if buf[..n.min(8192)].contains(&0) {
            return None;
        }
        decode_utf8_or_1252(&buf)
    };
    Some(normalize_text(&decoded))
}

// ====================================================================================================
// Markdown
// ====================================================================================================

fn unsafe_url(url: &str, allow_data_image: bool) -> bool {
    let t = url.trim_start();
    let starts = |p: &str| t.len() >= p.len() && t.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes());
    if starts("javascript:") || starts("vbscript:") {
        return true;
    }
    starts("data:") && !(allow_data_image && starts("data:image/") && !starts("data:image/svg"))
}

/// Markdown to a self-contained HTML document with theme-aware CSS. Raw HTML inside the Markdown is
/// escaped (the preview must stay inert), `javascript:` links are neutralised, relative links are kept.
pub fn markdown_to_html(md: &str, dark: bool) -> String {
    use pulldown_cmark::{html, CowStr, Event as Ev, Options, Parser, Tag};

    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let parser = Parser::new_ext(md, opts).map(|ev| match ev {
        Ev::Html(t) | Ev::InlineHtml(t) => Ev::Text(t),
        Ev::Start(Tag::Link { link_type, dest_url, title, id }) if unsafe_url(&dest_url, false) => {
            Ev::Start(Tag::Link { link_type, dest_url: CowStr::Borrowed("#"), title, id })
        }
        Ev::Start(Tag::Image { link_type, dest_url, title, id }) if unsafe_url(&dest_url, true) => {
            Ev::Start(Tag::Image { link_type, dest_url: CowStr::Borrowed("#"), title, id })
        }
        other => other,
    });

    let mut body = String::with_capacity(md.len() * 2 + 64);
    html::push_html(&mut body, parser);

    let (scheme, bg, fg, link) = if dark {
        ("dark", "#1e1e1e", "#e6e6e6", "#4a9eff")
    } else {
        ("light", "#ffffff", "#1f2328", "#0969da")
    };
    let mut out = String::with_capacity(body.len() + 1800);
    out.push_str("<!doctype html><meta charset=utf-8>");
    out.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data: https: http: file:; style-src 'unsafe-inline'\">");
    out.push_str("<style>");
    out.push_str(&format!(":root {{ color-scheme: {scheme}; }}\n"));
    out.push_str(&format!(
        "body {{ font: 15px/1.6 \"Segoe UI\", sans-serif; margin: 14px 18px; overflow-x: hidden; background: {bg}; color: {fg}; }}\n"
    ));
    out.push_str(
        "h1, h2 { border-bottom: 1px solid #8884; padding-bottom: .25em; }\n\
code, pre { font-family: \"Cascadia Mono\", Consolas, monospace; font-size: 13px; }\n\
pre { background: #8882; padding: 10px 12px; border-radius: 6px; overflow-x: auto; }\n\
code { background: #8882; padding: 1px 4px; border-radius: 4px; } pre code { background: none; padding: 0; }\n\
blockquote { margin-left: 0; padding-left: 12px; border-left: 3px solid #8886; opacity: .85; }\n\
table { border-collapse: collapse; } th, td { border: 1px solid #8886; padding: 4px 10px; }\n\
img { max-width: 100%; }\n\
li:has(> input[type=checkbox]) { list-style: none; margin-left: -1.2em; }\n",
    );
    out.push_str(&format!("a {{ color: {link}; }}\n::-webkit-scrollbar {{ display: none; }}\n</style><body>"));
    out.push_str(&body);
    out
}

// ====================================================================================================
// XML helpers
// ====================================================================================================

/// Upper bound on the decompressed bytes read from any single XML part.
const MAX_XML_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SHARED_STRINGS: usize = 200_000;
const MAX_COLS: u32 = 14;
const MAX_CELL_WIDTH: usize = 28;

type XmlReader<'a> = Reader<BufReader<io::Take<Box<dyn Read + 'a>>>>;

fn xml_reader<'a, R: Read + 'a>(r: R) -> XmlReader<'a> {
    let boxed: Box<dyn Read + 'a> = Box::new(r);
    let mut rd = Reader::from_reader(BufReader::with_capacity(64 * 1024, boxed.take(MAX_XML_BYTES)));
    rd.config_mut().check_end_names = false;
    rd
}

fn push_ref(out: &mut String, r: &BytesRef<'_>) {
    if r.is_char_ref() {
        if let Ok(Some(c)) = r.resolve_char_ref() {
            out.push(c);
        }
    } else if let Some(s) = quick_xml::escape::resolve_predefined_entity(r) {
        out.push_str(s);
    }
}

fn attr(e: &BytesStart<'_>, key: &str) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        if a.key.as_ref() == key {
            return a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned());
        }
    }
    None
}

/// Collapses runs of 3+ newlines to 2, trims the end, appends the truncation marker.
fn tidy(text: &str, truncated: bool) -> String {
    let t = text.replace('\r', "");
    let t = t.trim_end();
    let mut out = String::with_capacity(t.len() + 4);
    let mut nl = 0usize;
    for c in t.chars() {
        if c == '\n' {
            nl += 1;
            if nl > 2 {
                continue;
            }
        } else {
            nl = 0;
        }
        out.push(c);
    }
    if truncated {
        out.push_str("\n…");
    }
    out
}

// ====================================================================================================
// Office / OpenDocument
// ====================================================================================================

type Zip = zip::ZipArchive<BufReader<File>>;

/// Text extraction for docx/xlsx/pptx (and macro/template variants) and odt/ods/odp. Legacy binary formats
/// (.doc/.xls/.ppt) are not parsed and return `None`.
pub fn extract_office(path: &Path, max_lines: usize) -> Option<TextDoc> {
    let ext = extension_of(&path.to_string_lossy()).to_ascii_lowercase();
    let kind = match ext.as_str() {
        ".docx" | ".docm" | ".dotx" => 0,
        ".xlsx" | ".xlsm" | ".xltx" => 1,
        ".pptx" | ".pptm" | ".potx" | ".ppsx" => 2,
        ".odt" | ".ods" | ".odp" => 3,
        _ => return None,
    };
    let file = File::open(path).ok()?;
    let mut zip: Zip = zip::ZipArchive::new(BufReader::with_capacity(32 * 1024, file)).ok()?;
    match kind {
        0 => docx(&mut zip, max_lines),
        1 => xlsx(&mut zip, max_lines),
        2 => pptx(&mut zip, max_lines),
        _ => odf(&mut zip, &ext, max_lines),
    }
}

fn is_ws(s: &str) -> bool {
    s.chars().all(char::is_whitespace)
}

fn docx(zip: &mut Zip, max_lines: usize) -> Option<TextDoc> {
    let entry = zip.by_name("word/document.xml").ok()?;
    let mut rd = xml_reader(entry);
    let mut buf = Vec::with_capacity(4096);
    let mut sb = String::new();
    let (mut paragraphs, mut cell_depth) = (0usize, 0i32);
    let (mut in_t, mut truncated) = (false, false);

    loop {
        buf.clear();
        match rd.read_event_into(&mut buf).ok()? {
            Event::Start(e) => match e.local_name().into_inner() {
                "t" => in_t = true,
                "tab" => sb.push('\t'),
                "br" | "cr" => sb.push('\n'),
                "tc" => cell_depth += 1,
                _ => {}
            },
            Event::Empty(e) => match e.local_name().into_inner() {
                "tab" => sb.push('\t'),
                "br" | "cr" => sb.push('\n'),
                _ => {}
            },
            Event::Text(t) if in_t => sb.push_str(&t),
            Event::CData(t) if in_t => sb.push_str(&t),
            Event::GeneralRef(r) if in_t => push_ref(&mut sb, &r),
            Event::End(e) => {
                match e.local_name().into_inner() {
                    "t" => in_t = false,
                    "p" => {
                        sb.push(if cell_depth > 0 { ' ' } else { '\n' });
                        paragraphs += 1;
                        if paragraphs >= max_lines {
                            truncated = true;
                        }
                    }
                    "tc" => {
                        cell_depth -= 1;
                        sb.push_str(" │ ");
                    }
                    "tr" => sb.push('\n'),
                    _ => {}
                }
                if truncated {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Some(TextDoc {
        text: tidy(&sb, truncated),
        caption: format!("Word · {}{} paragraph(s) · Space to open", paragraphs, if truncated { "+" } else { "" }),
    })
}

/// .NET "G10" formatting of a double.
fn fmt_g10(d: f64) -> String {
    if d == 0.0 {
        return "0".to_string();
    }
    let sci = format!("{:.9e}", d); // e.g. -1.234567890e5
    let (mant, exp) = match sci.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => return sci,
    };
    if !(-5..10).contains(&exp) {
        let mut m = mant.to_string();
        if m.contains('.') {
            while m.ends_with('0') {
                m.pop();
            }
            if m.ends_with('.') {
                m.pop();
            }
        }
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{m}E{sign}{:02}", exp.abs());
    }
    let prec = (9 - exp).max(0) as usize;
    let mut s = format!("{:.*}", prec, d);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

fn parse_ref(reference: &str) -> Option<(u32, u32)> {
    let mut col: u64 = 0;
    let mut i = 0;
    for (idx, c) in reference.char_indices() {
        if !c.is_alphabetic() {
            break;
        }
        col = (col * 26 + (c.to_ascii_uppercase() as u64).wrapping_sub('A' as u64).wrapping_add(1)).min(1_000_000);
        i = idx + c.len_utf8();
    }
    if i == 0 || i >= reference.len() {
        return None;
    }
    let row: u32 = reference[i..].trim().parse().ok()?;
    Some((row, col as u32))
}

fn column_name(mut col: u32) -> String {
    let mut v = Vec::new();
    while col > 0 {
        v.push((b'A' + ((col - 1) % 26) as u8) as char);
        col = (col - 1) / 26;
    }
    v.iter().rev().collect()
}

fn pad_right(out: &mut String, s: &str, width: usize) {
    out.push_str(s);
    for _ in s.chars().count()..width {
        out.push(' ');
    }
}

fn xlsx(zip: &mut Zip, max_lines: usize) -> Option<TextDoc> {
    // sheet names
    let mut sheet_names: Vec<String> = Vec::new();
    if let Ok(wb) = zip.by_name("xl/workbook.xml") {
        let mut rd = xml_reader(wb);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match rd.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().into_inner() == "sheet" => {
                    sheet_names.push(attr(&e, "name").unwrap_or_default());
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }

    // shared strings
    let mut shared: Vec<String> = Vec::new();
    if let Ok(ss) = zip.by_name("xl/sharedStrings.xml") {
        let mut rd = xml_reader(ss);
        let mut buf = Vec::new();
        let (mut in_si, mut in_t) = (false, false);
        let mut cur = String::new();
        while shared.len() < MAX_SHARED_STRINGS {
            buf.clear();
            match rd.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => match e.local_name().into_inner() {
                    "si" => {
                        in_si = true;
                        cur.clear();
                    }
                    "t" if in_si => in_t = true,
                    _ => {}
                },
                Ok(Event::Empty(e)) if e.local_name().into_inner() == "si" => shared.push(String::new()),
                Ok(Event::Text(t)) if in_t => cur.push_str(&t),
                Ok(Event::CData(t)) if in_t => cur.push_str(&t),
                Ok(Event::GeneralRef(r)) if in_t => push_ref(&mut cur, &r),
                Ok(Event::End(e)) => match e.local_name().into_inner() {
                    "t" => in_t = false,
                    "si" => {
                        in_si = false;
                        in_t = false;
                        shared.push(std::mem::take(&mut cur));
                    }
                    _ => {}
                },
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }

    // first worksheet
    let sheet_path = if zip.by_name("xl/worksheets/sheet1.xml").is_ok() {
        "xl/worksheets/sheet1.xml".to_string()
    } else {
        zip.file_names()
            .filter_map(|n| n.ok())
            .filter(|n| n.starts_with("xl/worksheets/sheet"))
            .min()
            .map(|n| n.into_owned())?
    };

    let mut rows: BTreeMap<u32, BTreeMap<u32, String>> = BTreeMap::new();
    let mut max_col = 0u32;
    let mut truncated = false;

    {
        let entry = zip.by_name(&sheet_path).ok()?;
        let mut rd = xml_reader(entry);
        let mut buf = Vec::with_capacity(4096);
        'outer: loop {
            buf.clear();
            let ev = rd.read_event_into(&mut buf).ok()?;
            let (e, empty) = match ev {
                Event::Start(e) if e.local_name().into_inner() == "c" => (e.into_owned(), false),
                Event::Empty(e) if e.local_name().into_inner() == "c" => (e.into_owned(), true),
                Event::Eof => break,
                _ => continue,
            };
            let reference = attr(&e, "r").unwrap_or_default();
            let ty = attr(&e, "t");
            let Some((row, col)) = parse_ref(&reference) else {
                continue;
            };
            if row as usize > max_lines {
                truncated = true;
                break 'outer;
            }

            let mut v: Option<String> = None;
            let mut inline = String::new();
            if !empty {
                let mut mode = 0u8; // 1 = v, 2 = t
                let mut b2 = Vec::with_capacity(256);
                loop {
                    b2.clear();
                    match rd.read_event_into(&mut b2).ok()? {
                        Event::Start(x) => match x.local_name().into_inner() {
                            "v" => {
                                mode = 1;
                                v = Some(String::new());
                            }
                            "t" => mode = 2,
                            _ => {}
                        },
                        Event::Empty(x) if x.local_name().into_inner() == "v" => v = Some(String::new()),
                        Event::Text(t) => match mode {
                            1 => v.get_or_insert_with(String::new).push_str(&t),
                            2 => inline.push_str(&t),
                            _ => {}
                        },
                        Event::CData(t) => {
                            let s = t.to_string();
                            match mode {
                                1 => v.get_or_insert_with(String::new).push_str(&s),
                                2 => inline.push_str(&s),
                                _ => {}
                            }
                        }
                        Event::GeneralRef(r) => match mode {
                            1 => push_ref(v.get_or_insert_with(String::new), &r),
                            2 => push_ref(&mut inline, &r),
                            _ => {}
                        },
                        Event::End(x) => match x.local_name().into_inner() {
                            "c" => break,
                            "v" | "t" => mode = 0,
                            _ => {}
                        },
                        Event::Eof => break 'outer,
                        _ => {}
                    }
                }
            }
            let inline_opt = if inline.is_empty() { None } else { Some(inline) };

            let shared_idx = v
                .as_deref()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .filter(|&i| i >= 0 && (i as usize) < shared.len());
            let text: String = match (ty.as_deref(), shared_idx) {
                (Some("s"), Some(i)) => shared[i as usize].clone(),
                (Some("inlineStr"), _) => inline_opt.unwrap_or_default(),
                (Some("b"), _) => (if v.as_deref() == Some("1") { "TRUE" } else { "FALSE" }).to_string(),
                _ => match v {
                    Some(s) => match s.trim().parse::<f64>() {
                        Ok(d) if d.is_finite() => fmt_g10(d),
                        _ => s,
                    },
                    None => inline_opt.unwrap_or_default(),
                },
            };

            if text.is_empty() || col > MAX_COLS || col == 0 {
                continue;
            }
            let text = text.replace(['\n', '\r'], " ");
            rows.entry(row).or_default().insert(col, text);
            max_col = max_col.max(col);
        }
    }

    let first = sheet_names.first().cloned().unwrap_or_else(|| "Sheet1".to_string());
    if rows.is_empty() {
        return Some(TextDoc { text: "(empty sheet)".to_string(), caption: format!("Excel · {first}") });
    }

    let mut widths = vec![0usize; max_col as usize + 1];
    for cells in rows.values() {
        for (&c, t) in cells {
            widths[c as usize] = MAX_CELL_WIDTH.min(widths[c as usize].max(t.chars().count()));
        }
    }

    let mut out = String::with_capacity(rows.len() * (max_col as usize * 12 + 8) + 64);
    out.push_str("     ");
    for c in 1..=max_col {
        out.push(' ');
        pad_right(&mut out, &column_name(c), widths[c as usize].max(1));
        out.push_str("  ");
    }
    out.push('\n');
    for (row_num, cells) in &rows {
        out.push_str(&format!("{:>4} ", row_num));
        for c in 1..=max_col {
            let w = widths[c as usize];
            let t = cells.get(&c).map(String::as_str).unwrap_or("");
            out.push(' ');
            if t.chars().count() > w {
                let mut cut: String = t.chars().take(w.saturating_sub(1)).collect();
                cut.push('…');
                pad_right(&mut out, &cut, w.max(1));
            } else {
                pad_right(&mut out, t, w.max(1));
            }
            out.push_str("  ");
        }
        out.push('\n');
    }
    if truncated {
        out.push_str("…\n");
    }
    let more = if sheet_names.len() > 1 { format!(" (+{} more sheets)", sheet_names.len() - 1) } else { String::new() };
    Some(TextDoc { text: out, caption: format!("Excel · {first}{more} · Space to open") })
}

fn pptx(zip: &mut Zip, max_lines: usize) -> Option<TextDoc> {
    let mut slides: Vec<(String, u64)> = zip
        .file_names()
        .filter_map(|n| n.ok())
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .map(|n| {
            let base = n.rsplit('/').next().unwrap_or(&n);
            let stem = base.strip_suffix(".xml").unwrap_or(base);
            let num = stem.strip_prefix("slide").and_then(|s| s.parse::<u64>().ok()).unwrap_or(u64::MAX);
            (n.into_owned(), num)
        })
        .collect();
    if slides.is_empty() {
        return None;
    }
    slides.sort_by_key(|s| s.1);

    let mut sb = String::new();
    let (mut lines, mut shown) = (0usize, 0usize);
    for (name, n) in &slides {
        if lines >= max_lines {
            break;
        }
        sb.push_str("── Slide ");
        sb.push_str(&n.to_string());
        sb.push_str(" ──\n");
        let entry = zip.by_name(name).ok()?;
        let mut rd = xml_reader(entry);
        let mut buf = Vec::with_capacity(4096);
        let mut in_t = false;
        loop {
            buf.clear();
            match rd.read_event_into(&mut buf).ok()? {
                Event::Start(e) => match e.local_name().into_inner() {
                    "t" => in_t = true,
                    "br" => sb.push('\n'),
                    _ => {}
                },
                Event::Empty(e) if e.local_name().into_inner() == "br" => sb.push('\n'),
                Event::Text(t) if in_t => sb.push_str(&t),
                Event::CData(t) if in_t => sb.push_str(&t),
                Event::GeneralRef(r) if in_t => push_ref(&mut sb, &r),
                Event::End(e) => match e.local_name().into_inner() {
                    "t" => in_t = false,
                    "p" => {
                        sb.push('\n');
                        lines += 1;
                    }
                    _ => {}
                },
                Event::Eof => break,
                _ => {}
            }
        }
        sb.push('\n');
        shown += 1;
    }

    Some(TextDoc {
        text: tidy(&sb, shown < slides.len()),
        caption: format!("PowerPoint · {} slide(s) · Space to open", slides.len()),
    })
}

fn odf(zip: &mut Zip, ext: &str, max_lines: usize) -> Option<TextDoc> {
    let entry = zip.by_name("content.xml").ok()?;
    let mut rd = xml_reader(entry);
    let mut buf = Vec::with_capacity(4096);
    let mut sb = String::new();
    let mut lines = 0usize;
    let mut truncated = false;
    let odp = ext == ".odp";

    while !truncated {
        buf.clear();
        match rd.read_event_into(&mut buf).ok()? {
            Event::Text(t) => {
                if !is_ws(&t) {
                    sb.push_str(&t);
                }
            }
            Event::CData(t) => sb.push_str(&t),
            Event::GeneralRef(r) => push_ref(&mut sb, &r),
            Event::Start(e) | Event::Empty(e) => match e.local_name().into_inner() {
                "tab" => sb.push('\t'),
                "line-break" => sb.push('\n'),
                "s" => sb.push(' '),
                "page" if odp => sb.push_str("── Slide ──\n"),
                _ => {}
            },
            Event::End(e) => match e.local_name().into_inner() {
                "p" | "h" | "table-row" => {
                    sb.push('\n');
                    lines += 1;
                    truncated = lines >= max_lines;
                }
                "table-cell" => sb.push('\t'),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    let kind = match ext {
        ".odt" => "Writer",
        ".ods" => "Calc",
        _ => "Impress",
    };
    Some(TextDoc { text: tidy(&sb, truncated), caption: format!("OpenDocument {kind} · Space to open") })
}

// ====================================================================================================
// Archives
// ====================================================================================================

const MAX_ENTRIES: usize = 50_000;
const MAX_STREAM_ARCHIVE_BYTES: u64 = 300 * 1024 * 1024;
/// Decompression budget when a compressed stream has to be walked to list it.
const MAX_STREAM_DECOMPRESSED: u64 = 1024 * 1024 * 1024;
/// In-memory budget for formats (xz) whose decoder only writes into a sink.
const MAX_XZ_MEMORY: usize = 128 * 1024 * 1024;

enum ArcErr {
    Unsupported,
    Other(String),
}

impl From<io::Error> for ArcErr {
    fn from(e: io::Error) -> Self {
        ArcErr::Other(e.to_string())
    }
}

struct Tree {
    name: Vec<String>,
    is_dir: Vec<bool>,
    size: Vec<u64>,
    parent: Vec<u32>,
    depth: Vec<u32>,
    children: Vec<Vec<u32>>,
    lookup: HashMap<(u32, String), u32>,
    entries: usize,
    truncated: bool,
}

impl Tree {
    fn new() -> Self {
        Tree {
            name: vec![String::new()],
            is_dir: vec![true],
            size: vec![0],
            parent: vec![0],
            depth: vec![0],
            children: vec![Vec::new()],
            lookup: HashMap::new(),
            entries: 0,
            truncated: false,
        }
    }

    /// Returns false once the entry budget is used up.
    fn add(&mut self, path: &str, is_dir: bool, size: u64) -> bool {
        if self.entries >= MAX_ENTRIES {
            self.truncated = true;
            return false;
        }
        self.entries += 1;
        let mut node = 0u32;
        let mut parts = path.split(['/', '\\']).filter(|p| !p.is_empty()).peekable();
        let mut any = false;
        while let Some(part) = parts.next() {
            any = true;
            let last = parts.peek().is_none();
            let key = if part.is_ascii() { part.to_ascii_lowercase() } else { part.to_lowercase() };
            let existing = self.lookup.get(&(node, key.clone())).copied();
            node = match existing {
                Some(c) => {
                    if !last {
                        self.is_dir[c as usize] = true;
                    }
                    c
                }
                None => {
                    let idx = self.name.len() as u32;
                    self.name.push(part.to_string());
                    self.is_dir.push(!last || is_dir);
                    self.size.push(0);
                    self.parent.push(node);
                    self.depth.push(self.depth[node as usize] + 1);
                    self.children.push(Vec::new());
                    self.children[node as usize].push(idx);
                    self.lookup.insert((node, key), idx);
                    idx
                }
            };
        }
        if any && !is_dir {
            self.size[node as usize] = size;
        }
        true
    }

    /// Folder sizes and totals. Children always have larger indices than their parent, so one reverse pass does it.
    fn sum(&mut self) -> (u64, usize, usize) {
        let n = self.name.len();
        for i in 1..n {
            if self.is_dir[i] {
                self.size[i] = 0;
            }
        }
        let (mut files, mut folders) = (0usize, 0usize);
        for i in (1..n).rev() {
            if self.is_dir[i] {
                folders += 1;
            } else {
                files += 1;
            }
            let p = self.parent[i] as usize;
            self.size[p] = self.size[p].saturating_add(self.size[i]);
        }
        (self.size[0], files, folders)
    }

    fn widest(&self) -> usize {
        (1..self.name.len())
            .map(|i| (self.depth[i] as usize - 1) * 2 + self.name[i].chars().count() + usize::from(self.is_dir[i]))
            .max()
            .unwrap_or(0)
    }

    fn sorted_children(&self, node: u32) -> Vec<u32> {
        let mut v = self.children[node as usize].clone();
        v.sort_by_cached_key(|&c| (!self.is_dir[c as usize], self.name[c as usize].to_lowercase()));
        v
    }
}

/// Binary-size text like "1.5 MB".
fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        return format!("{bytes} B");
    }
    let s = format!("{:.1}", v);
    format!("{} {}", s.strip_suffix(".0").unwrap_or(&s), UNITS[i])
}

/// Integer with thousands separators.
fn n0(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn is_password_msg(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("password") || m.contains("encrypt") || m.contains("aes")
}

/// Walks a tar stream into the tree. An error after the first entries means a cut or damaged stream: keep what was read.
fn walk_tar<R: Read>(entries: tar::Entries<'_, R>, tree: &mut Tree) -> Result<(), ArcErr> {
    for e in entries {
        match e {
            Ok(entry) => {
                let is_dir = entry.header().entry_type().is_dir();
                let size = entry.header().size().unwrap_or(0);
                let p = entry.path_bytes();
                let path = String::from_utf8_lossy(&p);
                if !tree.add(&path, is_dir, size) {
                    break;
                }
            }
            Err(err) => {
                if tree.entries == 0 {
                    return Err(ArcErr::Other(err.to_string()));
                }
                tree.truncated = true;
                break;
            }
        }
    }
    Ok(())
}

fn read_tar<R: Read>(reader: R, tree: &mut Tree) -> Result<(), ArcErr> {
    let mut ar = tar::Archive::new(reader);
    let entries = ar.entries().map_err(|e| ArcErr::Other(e.to_string()))?;
    walk_tar(entries, tree)
}

struct CapWriter {
    buf: Vec<u8>,
    cap: usize,
}

impl Write for CapWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let room = self.cap.saturating_sub(self.buf.len());
        if room == 0 {
            return Err(io::Error::other("capacity reached"));
        }
        let n = room.min(data.len());
        self.buf.extend_from_slice(&data[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn open_buffered(path: &Path) -> io::Result<BufReader<File>> {
    Ok(BufReader::with_capacity(64 * 1024, File::open(path)?))
}

fn strip_ext<'a>(name: &'a str, ext: &str) -> &'a str {
    if name.len() >= ext.len() && name.is_char_boundary(name.len() - ext.len()) && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext) {
        &name[..name.len() - ext.len()]
    } else {
        name
    }
}

fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn xz_to_vec(path: &Path) -> Result<Vec<u8>, ArcErr> {
    let mut input = open_buffered(path)?;
    let mut w = CapWriter { buf: Vec::new(), cap: MAX_XZ_MEMORY };
    match lzma_rs::xz_decompress(&mut input, &mut w) {
        Ok(()) => Ok(w.buf),
        // Hitting the cap (or a damaged tail) still leaves a usable prefix.
        Err(_) if !w.buf.is_empty() => Ok(w.buf),
        Err(e) => Err(ArcErr::Other(e.to_string())),
    }
}

fn load_zip(path: &Path, tree: &mut Tree) -> Result<(), ArcErr> {
    let f = open_buffered(path)?;
    let archive = match zip::ZipArchive::new(f) {
        Ok(a) => a,
        Err(zip::result::ZipError::Io(e)) => return Err(ArcErr::Other(e.to_string())),
        Err(zip::result::ZipError::InvalidArchive(_)) | Err(zip::result::ZipError::UnsupportedArchive(_)) => {
            return Err(ArcErr::Unsupported)
        }
        Err(e) => return Err(ArcErr::Other(e.to_string())),
    };
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index_data(i) else { continue };
        let is_dir = entry.is_dir();
        let size = entry.size();
        let ok = match entry.name() {
            Ok(n) => tree.add(&n, is_dir, size),
            Err(_) => tree.add(&String::from_utf8_lossy(entry.name_raw()), is_dir, size),
        };
        if !ok {
            break;
        }
    }
    Ok(())
}

fn load_7z(path: &Path, tree: &mut Tree) -> Result<(), ArcErr> {
    let mut f = open_buffered(path)?;
    match sevenz_rust2::Archive::read(&mut f, &sevenz_rust2::Password::empty()) {
        Ok(a) => {
            for e in &a.files {
                if !tree.add(e.name(), e.is_directory(), e.size()) {
                    break;
                }
            }
            Ok(())
        }
        Err(sevenz_rust2::Error::BadSignature(_)) => Err(ArcErr::Unsupported),
        Err(sevenz_rust2::Error::PasswordRequired) => Err(ArcErr::Other("password required".to_string())),
        Err(e) => Err(ArcErr::Other(e.to_string())),
    }
}

fn load_archive(path: &Path, tree: &mut Tree) -> Result<(), ArcErr> {
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = file_name.to_ascii_lowercase();
    let ends = |s: &[&str]| s.iter().any(|e| name.ends_with(e));
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let stream_kind = ends(&[".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tar.xz", ".txz", ".gz", ".bz2", ".xz"]);
    if stream_kind && file_len > MAX_STREAM_ARCHIVE_BYTES {
        return Err(ArcErr::Other(format!(
            "This compressed archive is larger than {} MB and would have to be fully decompressed to list it.",
            MAX_STREAM_ARCHIVE_BYTES / 1024 / 1024
        )));
    }

    if ends(&[".zip", ".jar", ".apk", ".nupkg", ".vsix", ".whl"]) {
        return load_zip(path, tree);
    }
    if ends(&[".7z"]) {
        return load_7z(path, tree);
    }
    if ends(&[".tar"]) {
        let mut ar = tar::Archive::new(open_buffered(path)?);
        let entries = ar.entries_with_seek().map_err(|e| ArcErr::Other(e.to_string()))?;
        return walk_tar(entries, tree);
    }
    if ends(&[".tar.gz", ".tgz"]) {
        let dec = flate2::read::MultiGzDecoder::new(open_buffered(path)?);
        return read_tar(dec.take(MAX_STREAM_DECOMPRESSED), tree);
    }
    if ends(&[".tar.bz2", ".tbz2"]) {
        let dec = bzip2::read::MultiBzDecoder::new(open_buffered(path)?);
        return read_tar(dec.take(MAX_STREAM_DECOMPRESSED), tree);
    }
    if ends(&[".tar.xz", ".txz"]) {
        return read_tar(io::Cursor::new(xz_to_vec(path)?), tree);
    }
    if ends(&[".gz"]) {
        // A tarball with a short name, or a single compressed file.
        let mut dec = flate2::read::MultiGzDecoder::new(open_buffered(path)?);
        let mut head = [0u8; 512];
        let got = read_full(&mut dec, &mut head)?;
        if got == 512 && &head[257..262] == b"ustar" {
            let dec = flate2::read::MultiGzDecoder::new(open_buffered(path)?);
            return read_tar(dec.take(MAX_STREAM_DECOMPRESSED), tree);
        }
        // Uncompressed size is stored in the last four bytes (modulo 4 GiB).
        let mut size = 0u64;
        if file_len >= 4 {
            let mut f = File::open(path)?;
            f.seek(io::SeekFrom::End(-4))?;
            let mut t = [0u8; 4];
            f.read_exact(&mut t)?;
            size = u32::from_le_bytes(t) as u64;
        }
        tree.add(strip_ext(&file_name, ".gz"), false, size);
        return Ok(());
    }
    if ends(&[".bz2"]) {
        let dec = bzip2::read::MultiBzDecoder::new(open_buffered(path)?);
        let size = io::copy(&mut dec.take(MAX_STREAM_DECOMPRESSED), &mut io::sink())?;
        tree.add(strip_ext(&file_name, ".bz2"), false, size);
        return Ok(());
    }
    if ends(&[".xz"]) {
        let data = xz_to_vec(path)?;
        tree.add(strip_ext(&file_name, ".xz"), false, data.len() as u64);
        return Ok(());
    }
    Err(ArcErr::Unsupported)
}

/// Lists an archive as an indented folder tree (folders first, sizes, totals) without extracting anything.
/// `max_entries` bounds the number of tree lines. `None` for formats that cannot be listed (rar, cab, iso, unknown).
pub fn list_archive(path: &Path, max_entries: usize) -> Option<TextDoc> {
    let kind = path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
    let mut tree = Tree::new();
    match load_archive(path, &mut tree) {
        Ok(()) => {}
        Err(ArcErr::Unsupported) => return None,
        Err(ArcErr::Other(msg)) => {
            let text = if is_password_msg(&msg) {
                "This archive is password protected, its file list cannot be shown.".to_string()
            } else {
                format!("Cannot list this archive: {msg}")
            };
            return Some(TextDoc { text, caption: format!("{kind} archive") });
        }
    }

    let (total, files, folders) = tree.sum();
    let mut sb = String::with_capacity(max_entries.min(tree.name.len()) * 48 + 128);
    sb.push_str(&format!("{} file(s), {} folder(s) · {} unpacked", n0(files), n0(folders), fmt_size(total)));
    if tree.truncated {
        sb.push_str(&format!(" · first {} entries only", n0(MAX_ENTRIES)));
    }
    sb.push_str("\n\n");

    let width = 64.min(20.max(tree.widest() + 2));
    let mut lines = 0usize;
    // iterative depth-first walk: (sorted children, next position); stack depth == tree depth
    let mut stack: Vec<(Vec<u32>, usize)> = vec![(tree.sorted_children(0), 0)];
    while let Some((kids, pos)) = stack.last_mut() {
        if *pos >= kids.len() {
            stack.pop();
            continue;
        }
        if lines >= max_entries {
            break;
        }
        let c = kids[*pos];
        *pos += 1;
        let ci = c as usize;
        let depth = stack.len() - 1;

        let mut label = String::with_capacity(depth * 2 + tree.name[ci].len() + 1);
        for _ in 0..depth * 2 {
            label.push(' ');
        }
        label.push_str(&tree.name[ci]);
        if tree.is_dir[ci] {
            label.push('\\');
        }
        if label.chars().count() > width {
            label = label.chars().take(width - 1).collect::<String>() + "…";
        }
        pad_right(&mut sb, &label, width);
        let sz = fmt_size(tree.size[ci]);
        for _ in sz.chars().count()..10 {
            sb.push(' ');
        }
        sb.push_str(&sz);
        sb.push('\n');
        lines += 1;

        if tree.is_dir[ci] && !tree.children[ci].is_empty() {
            stack.push((tree.sorted_children(c), 0));
        }
    }
    if lines >= max_entries {
        sb.push_str("…  (listing cut off here, open it with Space to see more)\n");
    }

    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Some(TextDoc { text: sb, caption: format!("{kind} archive · {} file(s) · {}", n0(files), fmt_size(len)) })
}
