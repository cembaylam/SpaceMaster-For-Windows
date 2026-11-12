use spacemaster::docs::*;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;
use zip::write::SimpleFileOptions;

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("docs_tests");
    fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn make_zip(path: &Path, parts: &[(&str, &str)]) {
    let mut z = zip::ZipWriter::new(File::create(path).unwrap());
    let opt = SimpleFileOptions::default();
    for (name, body) in parts {
        z.start_file(*name, opt).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap();
}

// ------------------------------------------------------------------ kinds

#[test]
fn kind_mapping() {
    assert_eq!(kind_of("a.PNG"), Kind::Image);
    assert_eq!(kind_of("clip.MkV"), Kind::Video);
    assert_eq!(kind_of("song.flac"), Kind::Audio);
    assert_eq!(kind_of("doc.PDF"), Kind::Pdf);
    assert_eq!(kind_of("x.tar"), Kind::Archive);
    assert_eq!(kind_of("x.nupkg"), Kind::Archive);
    assert_eq!(kind_of("report.docx"), Kind::Office);
    assert_eq!(kind_of("old.xls"), Kind::Office);
    assert_eq!(kind_of("main.rs"), Kind::Text);
    assert_eq!(kind_of(".gitignore"), Kind::Text);
    assert_eq!(kind_of("noext"), Kind::Other);
    assert_eq!(kind_of("trailing."), Kind::Other);
    assert_eq!(kind_of("a.unknownext"), Kind::Other);
    assert_eq!(kind_of(r"C:\dir.png\file"), Kind::Other);
}

#[test]
fn renderable() {
    assert!(is_renderable("a.md"));
    assert!(is_renderable("A.MARKDOWN"));
    assert!(is_renderable("p.HTM"));
    assert!(is_renderable("p.html"));
    assert!(!is_renderable("a.txt"));
    assert!(!is_renderable("md"));
}

// ------------------------------------------------------------------ hex

#[test]
fn hex_format() {
    assert_eq!(hex_dump(&[]), "(empty file)");
    let data: Vec<u8> = (0u8..20).collect();
    let s = hex_dump(&data);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "Offset    00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F   Decoded text");
    assert_eq!(lines[1], "--------  -- -- -- -- -- -- -- --  -- -- -- -- -- -- -- --   ------------");
    assert_eq!(lines[2], "00000000  00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F   ................");
    // 4 bytes present, 12 missing (3 blanks each), the extra gap after byte 8, then the 2-blank text separator
    let expected3 = format!("00000010  10 11 12 13 {}{}{}....", "   ".repeat(12), " ", "  ");
    assert_eq!(lines[3], expected3);
    assert!(s.ends_with('\n'));

    let t = hex_dump(b"Hello, World!\xFF");
    assert!(t.contains("48 65 6C 6C 6F 2C 20 57  6F 72 6C 64 21 FF"));
    assert!(t.contains("Hello, World!."));
}

#[test]
fn hex_256kb_is_fast() {
    let data: Vec<u8> = (0..256 * 1024).map(|i| (i * 7) as u8).collect();
    let t = Instant::now();
    let s = hex_dump(&data);
    assert!(t.elapsed().as_millis() < 250, "hex took {:?}", t.elapsed());
    assert_eq!(s.lines().count(), 2 + 256 * 1024 / 16);
}

// ------------------------------------------------------------------ head reads

#[test]
fn text_head_variants() {
    let p = tmp("plain.txt");
    fs::write(&p, "a\tb\r\nc").unwrap();
    assert_eq!(read_text_head(&p, 1000).unwrap(), "a    b\nc");

    let p = tmp("empty.txt");
    fs::write(&p, "").unwrap();
    assert_eq!(read_text_head(&p, 1000).unwrap(), "");

    let p = tmp("bin.dat");
    fs::write(&p, [b'a', 0, b'b']).unwrap();
    assert!(read_text_head(&p, 1000).is_none());

    let p = tmp("bom8.txt");
    fs::write(&p, b"\xEF\xBB\xBFh\xC3\xA9llo").unwrap();
    assert_eq!(read_text_head(&p, 1000).unwrap(), "héllo");

    let mut u16le = vec![0xFF, 0xFE];
    for u in "héllo €".encode_utf16() {
        u16le.extend_from_slice(&u.to_le_bytes());
    }
    let p = tmp("le.txt");
    fs::write(&p, &u16le).unwrap();
    assert_eq!(read_text_head(&p, 1000).unwrap(), "héllo €");

    let mut u16be = vec![0xFE, 0xFF];
    for u in "héllo".encode_utf16() {
        u16be.extend_from_slice(&u.to_be_bytes());
    }
    let p = tmp("be.txt");
    fs::write(&p, &u16be).unwrap();
    assert_eq!(read_text_head(&p, 1000).unwrap(), "héllo");

    // invalid UTF-8 falls back to Windows-1252
    let p = tmp("cp1252.txt");
    fs::write(&p, b"caf\xE9 \x80 \x93q\x94").unwrap();
    assert_eq!(read_text_head(&p, 1000).unwrap(), "café € \u{201C}q\u{201D}");
}

#[test]
fn text_head_never_splits_chars() {
    let p = tmp("cut.txt");
    fs::write(&p, "aé€😀b").unwrap(); // 1 + 2 + 3 + 4 + 1 bytes
    assert_eq!(read_text_head(&p, 2).unwrap(), "a");
    assert_eq!(read_text_head(&p, 4).unwrap(), "aé");
    assert_eq!(read_text_head(&p, 5).unwrap(), "aé");
    assert_eq!(read_text_head(&p, 6).unwrap(), "aé€");
    assert_eq!(read_text_head(&p, 9).unwrap(), "aé€");
    assert_eq!(read_text_head(&p, 10).unwrap(), "aé€😀");

    let mut u = vec![0xFF, 0xFE];
    for x in "a😀".encode_utf16() {
        u.extend_from_slice(&x.to_le_bytes());
    }
    let p = tmp("cut16.txt");
    fs::write(&p, &u).unwrap();
    assert_eq!(read_text_head(&p, 2 + 2 + 2).unwrap(), "a"); // lone high surrogate dropped
    assert_eq!(read_text_head(&p, 2 + 2 + 3).unwrap(), "a"); // odd byte dropped
}

#[test]
fn head_bytes_are_bounded() {
    let p = tmp("big.bin");
    fs::write(&p, vec![7u8; 100_000]).unwrap();
    assert_eq!(read_head_bytes(&p, 1000).unwrap().len(), 1000);
    assert_eq!(read_head_bytes(&p, 1_000_000).unwrap().len(), 100_000);
    assert!(read_head_bytes(&tmp("does-not-exist"), 10).is_none());
}

// ------------------------------------------------------------------ markdown

#[test]
fn markdown_rendering() {
    let md = "# Title\n\n<script>alert(1)</script>\n\nText <b>x</b> and [rel](docs/a.md) [bad](javascript:alert(1))\n\n\
| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n\n~~gone~~\n\n```rust\nfn main() {}\n```\n";
    let h = markdown_to_html(md, false);
    assert!(h.starts_with("<!doctype html>"));
    assert!(h.contains("<h1>Title</h1>"));
    assert!(!h.contains("<script>"));
    assert!(h.contains("&lt;script&gt;"));
    assert!(!h.contains("<b>x</b>"));
    assert!(h.contains("href=\"docs/a.md\""));
    assert!(!h.contains("javascript:"));
    assert!(h.contains("<table>"));
    assert!(h.contains("checkbox"));
    assert!(h.contains("<del>gone</del>"));
    assert!(h.contains("language-rust"));

    let light = markdown_to_html("x", false);
    let dark = markdown_to_html("x", true);
    assert!(light.contains("color-scheme: light"));
    assert!(dark.contains("color-scheme: dark"));
    assert_ne!(light, dark);
}

// ------------------------------------------------------------------ office

const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

#[test]
fn docx_text() {
    let doc = format!(
        "<?xml version=\"1.0\"?><w:document {W}><w:body>\
<w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space=\"preserve\"> World &amp; co</w:t></w:r></w:p>\
<w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t><w:br/><w:t>c</w:t></w:r></w:p>\
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>c1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>c2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
<w:p><w:r><w:t>last</w:t></w:r></w:p></w:body></w:document>"
    );
    let p = tmp("sample.docx");
    make_zip(&p, &[("word/document.xml", &doc)]);
    let d = extract_office(&p, 100).unwrap();
    assert_eq!(d.text, "Hello World & co\na\tb\nc\nc1  │ c2  │ \nlast");
    assert_eq!(d.caption, "Word · 5 paragraph(s) · Space to open");

    let d = extract_office(&p, 2).unwrap();
    assert!(d.text.ends_with("\n…"), "{:?}", d.text);
    assert_eq!(d.caption, "Word · 2+ paragraph(s) · Space to open");

    // legacy and non-zip inputs
    let legacy = tmp("old.doc");
    fs::write(&legacy, b"\xD0\xCF\x11\xE0 not a zip").unwrap();
    assert!(extract_office(&legacy, 10).is_none());
    let broken = tmp("broken.docx");
    fs::write(&broken, b"not a zip at all").unwrap();
    assert!(extract_office(&broken, 10).is_none());
}

#[test]
fn xlsx_grid() {
    let wb = "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheets>\
<sheet name=\"Data\" sheetId=\"1\"/><sheet name=\"Other\" sheetId=\"2\"/></sheets></workbook>";
    let ss = "<sst><si><t>Name</t></si><si><r><t>Al</t></r><r><t>ice</t></r></si><si><t>Qty</t></si></sst>";
    let sheet = "<worksheet><sheetData>\
<row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\" t=\"s\"><v>2</v></c><c r=\"C1\" t=\"inlineStr\"><is><t>inl</t></is></c></row>\
<row r=\"2\"><c r=\"A2\" t=\"s\"><v>1</v></c><c r=\"B2\"><v>3.50</v></c><c r=\"C2\" t=\"b\"><v>1</v></c></row>\
<row r=\"3\"><c r=\"B3\"><v>0.1234567890123</v></c><c r=\"D3\"><v>1E+15</v></c></row>\
<row r=\"50\"><c r=\"A50\"><v>9</v></c></row>\
</sheetData></worksheet>";
    let p = tmp("sample.xlsx");
    make_zip(&p, &[("xl/workbook.xml", wb), ("xl/sharedStrings.xml", ss), ("xl/worksheets/sheet1.xml", sheet)]);

    let d = extract_office(&p, 10).unwrap();
    assert_eq!(d.caption, "Excel · Data (+1 more sheets) · Space to open");
    let lines: Vec<&str> = d.text.lines().collect();
    // column widths: A=5 (Alice), B=11 (0.123456789), C=4, D=5 (1E+15); every cell is " " + padded + "  "
    let cell = |s: &str, w: usize| format!(" {:<w$}  ", s, w = w);
    let row = |n: &str, c: [&str; 4]| {
        format!("{:>4} {}{}{}{}", n, cell(c[0], 5), cell(c[1], 11), cell(c[2], 4), cell(c[3], 5))
    };
    assert_eq!(lines[0], format!("     {}{}{}{}", cell("A", 5), cell("B", 11), cell("C", 4), cell("D", 5)));
    assert_eq!(lines[1], row("1", ["Name", "Qty", "inl", ""]));
    assert_eq!(lines[2], row("2", ["Alice", "3.5", "TRUE", ""]));
    assert_eq!(lines[3], row("3", ["", "0.123456789", "", "1E+15"]));
    assert!(d.text.ends_with("…\n"), "row 50 is past the limit: {:?}", d.text);

    let d = extract_office(&p, 100).unwrap();
    assert!(d.text.lines().any(|l| l.starts_with("  50 ")));
    assert!(!d.text.ends_with("…\n"));

    let empty = tmp("empty.xlsx");
    make_zip(&empty, &[("xl/worksheets/sheet1.xml", "<worksheet><sheetData/></worksheet>")]);
    let d = extract_office(&empty, 10).unwrap();
    assert_eq!(d.text, "(empty sheet)");
    assert_eq!(d.caption, "Excel · Sheet1");
}

#[test]
fn pptx_slides() {
    let slide = |t: &str| {
        format!("<p:sld xmlns:a=\"a\" xmlns:p=\"p\"><a:p><a:r><a:t>{t}</a:t></a:r></a:p><a:p><a:r><a:t>second</a:t></a:r></a:p></p:sld>")
    };
    let (s1, s2, s10) = (slide("One"), slide("Two"), slide("Ten"));
    let p = tmp("sample.pptx");
    make_zip(
        &p,
        &[
            ("ppt/slides/slide10.xml", &s10),
            ("ppt/slides/slide2.xml", &s2),
            ("ppt/slides/slide1.xml", &s1),
            ("ppt/slides/_rels/slide1.xml.rels", "<x/>"),
        ],
    );
    let d = extract_office(&p, 100).unwrap();
    assert_eq!(
        d.text,
        "── Slide 1 ──\nOne\nsecond\n\n── Slide 2 ──\nTwo\nsecond\n\n── Slide 10 ──\nTen\nsecond"
    );
    assert_eq!(d.caption, "PowerPoint · 3 slide(s) · Space to open");

    let d = extract_office(&p, 2).unwrap();
    assert!(d.text.contains("Slide 1") && !d.text.contains("Slide 2"));
    assert!(d.text.ends_with("\n…"));
}

#[test]
fn odt_text() {
    let content = "<office:document-content xmlns:office=\"o\" xmlns:text=\"t\">\n<office:body>\n<office:text>\n\
<text:h>Heading</text:h>\n<text:p>one<text:tab/>two<text:line-break/>three</text:p>\n<text:p>a &amp; b</text:p>\n\
</office:text>\n</office:body>\n</office:document-content>";
    let p = tmp("sample.odt");
    make_zip(&p, &[("content.xml", content)]);
    let d = extract_office(&p, 100).unwrap();
    assert_eq!(d.text, "Heading\none\ttwo\nthree\na & b");
    assert_eq!(d.caption, "OpenDocument Writer · Space to open");
}

// ------------------------------------------------------------------ archives

#[test]
fn zip_tree() {
    let p = tmp("tree.zip");
    let mut z = zip::ZipWriter::new(File::create(&p).unwrap());
    let opt = SimpleFileOptions::default();
    z.start_file("readme.txt", opt).unwrap();
    z.write_all(&[b'x'; 2048]).unwrap();
    z.add_directory("src/", opt).unwrap();
    z.start_file("src/main.rs", opt).unwrap();
    z.write_all(&[b'y'; 100]).unwrap();
    z.start_file("src/deep/a.bin", opt).unwrap();
    z.write_all(&[0u8; 1024]).unwrap();
    z.start_file("Zed.txt", opt).unwrap();
    z.finish().unwrap();

    let d = list_archive(&p, 1000).unwrap();
    assert_eq!(d.caption.split(" · ").next().unwrap(), "ZIP archive");
    let lines: Vec<&str> = d.text.lines().collect();
    assert_eq!(lines[0], "4 file(s), 2 folder(s) · 3.1 KB unpacked");
    assert_eq!(lines[1], "");
    // folders first, files after, each alphabetical; folder size is the sum below it
    let body: Vec<String> = lines[2..].iter().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
    assert_eq!(
        body,
        vec![
            "src\\ 1.1 KB",
            "deep\\ 1 KB",
            "a.bin 1 KB",
            "main.rs 100 B",
            "readme.txt 2 KB",
            "Zed.txt 0 B",
        ]
    );
    assert!(lines[2].starts_with("src\\ "));
    assert!(lines[3].starts_with("  deep\\ "));
    assert!(lines[4].starts_with("    a.bin "));

    let cut = list_archive(&p, 2).unwrap();
    assert!(cut.text.contains("listing cut off"));
}

#[test]
fn zip_20k_entries_is_fast() {
    let p = tmp("many.zip");
    {
        let mut z = zip::ZipWriter::new(std::io::BufWriter::new(File::create(&p).unwrap()));
        let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for i in 0..20_000 {
            z.start_file(format!("dir{}/sub{}/file{}.txt", i % 50, i % 7, i), opt).unwrap();
        }
        z.finish().unwrap();
    }
    let t = Instant::now();
    let d = list_archive(&p, 500).unwrap();
    let el = t.elapsed();
    assert!(el.as_millis() < 1500, "listing took {el:?}");
    assert!(d.text.starts_with("20,000 file(s), "), "{}", d.text.lines().next().unwrap());
    assert!(d.text.contains("listing cut off"));
}

#[test]
fn tar_gz_and_tar() {
    let mut raw = Vec::new();
    {
        let mut b = tar::Builder::new(&mut raw);
        for (name, len) in [("pkg/bin/tool", 3000usize), ("pkg/README", 10)] {
            let mut h = tar::Header::new_gnu();
            h.set_size(len as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, &vec![b'z'; len][..]).unwrap();
        }
        b.finish().unwrap();
    }
    let plain = tmp("pack.tar");
    fs::write(&plain, &raw).unwrap();
    let gz = tmp("pack.tar.gz");
    {
        let mut e = flate2::write::GzEncoder::new(File::create(&gz).unwrap(), flate2::Compression::fast());
        e.write_all(&raw).unwrap();
        e.finish().unwrap();
    }
    let tgz = tmp("pack.tgz");
    fs::copy(&gz, &tgz).unwrap();

    for p in [&plain, &gz, &tgz] {
        let d = list_archive(p, 100).unwrap();
        let first = d.text.lines().next().unwrap();
        assert_eq!(first, "2 file(s), 2 folder(s) · 2.9 KB unpacked", "{p:?}");
        assert!(d.text.contains("pkg\\"));
        assert!(d.text.contains("  bin\\"));
        assert!(d.text.contains("tool"));
    }

    // plain .gz holding a single file
    let single = tmp("notes.txt.gz");
    {
        let mut e = flate2::write::GzEncoder::new(File::create(&single).unwrap(), flate2::Compression::fast());
        e.write_all(&[b'q'; 5000]).unwrap();
        e.finish().unwrap();
    }
    let d = list_archive(&single, 100).unwrap();
    assert!(d.text.starts_with("1 file(s), 0 folder(s) · 4.9 KB unpacked"));
    assert!(d.text.contains("notes.txt"));
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// A 7z with a plain (uncompressed) header describing one folder and two empty files: no packed streams needed.
fn make_7z(path: &Path) {
    let names = ["docs", "docs/readme.txt", "empty.txt"];
    let mut name_bytes = vec![0u8];
    for n in names {
        for u in n.encode_utf16() {
            name_bytes.extend_from_slice(&u.to_le_bytes());
        }
        name_bytes.extend_from_slice(&[0, 0]);
    }
    let mut header = vec![0x01, 0x05, names.len() as u8];
    header.extend_from_slice(&[0x0E, 1, 0b1110_0000]); // all three entries have no stream
    header.extend_from_slice(&[0x0F, 1, 0b0110_0000]); // ...and two of them are empty files (the first is a folder)
    header.push(0x11);
    header.push(name_bytes.len() as u8);
    header.extend_from_slice(&name_bytes);
    header.extend_from_slice(&[0x00, 0x00]);

    let mut start = Vec::new();
    start.extend_from_slice(&0u64.to_le_bytes());
    start.extend_from_slice(&(header.len() as u64).to_le_bytes());
    start.extend_from_slice(&crc32(&header).to_le_bytes());

    let mut out = vec![b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C, 0, 4];
    out.extend_from_slice(&crc32(&start).to_le_bytes());
    out.extend_from_slice(&start);
    out.extend_from_slice(&header);
    fs::write(path, out).unwrap();
}

#[test]
fn sevenz_listing() {
    let p = tmp("sample.7z");
    make_7z(&p);
    let d = list_archive(&p, 100).unwrap();
    assert!(d.text.starts_with("2 file(s), 1 folder(s) · 0 B unpacked"), "{}", d.text);
    let body: Vec<String> = d.text.lines().skip(2).map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
    assert_eq!(body, vec!["docs\\ 0 B", "readme.txt 0 B", "empty.txt 0 B"]);
    assert!(d.caption.starts_with("7Z archive"));
}

#[test]
fn unsupported_and_broken_archives() {
    for n in ["a.rar", "a.cab", "a.iso", "a.unknown"] {
        let p = tmp(n);
        fs::write(&p, b"whatever").unwrap();
        assert!(list_archive(&p, 10).is_none(), "{n}");
    }
    let bad7 = tmp("bad.7z");
    fs::write(&bad7, b"this is not a 7z file at all, really").unwrap();
    assert!(list_archive(&bad7, 10).is_none());
    assert!(list_archive(&tmp("missing.zip"), 10).is_some_and(|d| d.text.starts_with("Cannot list this archive")));
}
