use spacemaster::highlight::*;
use std::time::Instant;

fn check_invariants(text: &str, lang: Lang) -> Vec<Span> {
    let spans = tokenize(text, lang);
    let mut pos = 0usize;
    for s in &spans {
        assert!(s.len > 0, "{lang:?}: empty span in {text:?}");
        assert_eq!(s.start, pos, "{lang:?}: gap or overlap at {pos} in {text:?}");
        assert!(text.is_char_boundary(s.start), "{lang:?}: start {} not on a char boundary", s.start);
        pos += s.len;
        assert!(text.is_char_boundary(pos), "{lang:?}: end {pos} not on a char boundary");
    }
    assert_eq!(pos, text.len(), "{lang:?}: spans do not cover the text");
    spans
}

/// The (text, kind) pairs of the non-plain spans.
fn toks(text: &str, lang: Lang) -> Vec<(&str, Tok)> {
    check_invariants(text, lang)
        .into_iter()
        .filter(|s| s.kind != Tok::Plain)
        .map(|s| (&text[s.start..s.start + s.len], s.kind))
        .collect()
}

fn has(text: &str, lang: Lang, piece: &str, kind: Tok) -> bool {
    toks(text, lang).iter().any(|(t, k)| *t == piece && *k == kind)
}

fn sample(lang: Lang) -> &'static str {
    match lang {
        Lang::CSharp => "#region x\nusing System;\r\nnamespace Ä.Ö { public class Müller { string s = @\"a\"\"b\nc\"; var t = $\"{x}ü\"; /* c */ int n = 0x1F; // z\n} }\n\"unterminated ü\n/* open",
        Lang::JavaScript => "const é = `tpl\nü` + 'x\\\n'; function f() { return 1.5e3 } // ç\n/* open",
        Lang::Java => "public class Ü { final int x = 1; String s = \"a\\\"b\"; }\n// \u{1F600}\n\"open",
        Lang::Cpp => "#include <x>\r\nint main() { return 0; } /* ü */ \"s\\\n#if",
        Lang::Go => "package main\nvar s = `raw\nü`\nfunc f() {}\n`open",
        Lang::Rust => "fn main<'a>() { let s = \"ü\\\"\"; /* x */ 0u8 } // é\n\"open",
        Lang::Python => "def f(x):\n    \"\"\"doc ü\"\"\"\n    return 'a' # é\n'''open",
        Lang::Shell => "if [ -n \"$HOME\" ]; then echo `date` ü; fi # c\n'open",
        Lang::PowerShell => "function Get-X { param($a) <# c\nü #> $a } # z\n\"open",
        Lang::Sql => "SELECT a, 'ü' FROM t -- c\n/* x */ WHERE id = 1;\n'open",
        Lang::Batch => "@echo off\r\nREM comment\r\n:: other ü\r\nset X=\"a b\"\r\n\"open",
        Lang::Json => "{\"é\": [1, 2.5e3, true, null], \"k\" : \"v\\u00fc\"} // c\n/* open\n\"x",
        Lang::Xml => "<?xml version=\"1.0\"?>\r\n<!-- c --><a:b x=\"ü\" y='1'>t &amp; ü<![CDATA[x]]></a:b><c/><\n<open attr=\"x",
        Lang::Html => "<!DOCTYPE html><html><script>var é = 1; // c\n</script><style>a { color: #fff; }</style><p class=x>ü</p><script>unterminated",
        Lang::Css => "/* c */ .a-ü, #b > p { color: #1a2B3c; margin: 0 .5em; content: \"x\\\"y\"; } @media (x) { }\n\"open\n/* open",
        Lang::KeyValue => "[sec]\r\nkey = \"ü\" # c\r\n- item: 1.5\n  name: true\n# comment\n; other\nbare\n[open",
        Lang::Markdown => "# Başlık\r\n- item `code ü` text\n1. one\n> quote\n```rust\nfn x() {}\n```\n`open\n~~~\nopen fence",
    }
}

#[test]
fn invariants_on_samples() {
    for lang in Lang::ALL {
        let text = sample(lang);
        check_invariants(text, lang);
        // Every prefix and suffix, including cuts that fall on multi-byte characters.
        for (i, _) in text.char_indices() {
            check_invariants(&text[..i], lang);
            check_invariants(&text[i..], lang);
        }
    }
}

#[test]
fn invariants_on_degenerate_input() {
    let inputs = [
        "", " ", "\n", "\r\n", "\"", "'", "`", "/*", "//", "<", "<!--", "<![CDATA[", "<a", "<a b='", "&", "&amp", "#", "@\"", "$@", "\\",
        "\"\\", "ü", "é\"é", "\u{1F600}", "<script>", "<style>/*", "[", "- ", "```", ".5", "1.", "\u{00A0}x\u{00A0}", "a\u{2003}b",
    ];
    for lang in Lang::ALL {
        for text in inputs {
            check_invariants(text, lang);
        }
        assert!(tokenize("", lang).is_empty());
    }
}

#[test]
fn csharp_tokens() {
    let src = "#region R\nusing System;\npublic class Foo { string s = \"a\\\"b\"; int n = 42; // note\n  var v = @\"x\"\"y\nz\"; /* b */ }";
    let l = Lang::CSharp;
    assert!(has(src, l, "#region", Tok::Preproc));
    assert!(has(src, l, "using", Tok::Keyword));
    assert!(has(src, l, "public", Tok::Keyword));
    assert!(has(src, l, "System", Tok::Type));
    assert!(has(src, l, "Foo", Tok::Type));
    assert!(has(src, l, "\"a\\\"b\"", Tok::String));
    assert!(has(src, l, "42", Tok::Number));
    assert!(has(src, l, "// note", Tok::Comment));
    assert!(has(src, l, "@\"x\"\"y\nz\"", Tok::String));
    assert!(has(src, l, "/* b */", Tok::Comment));
    // ALL-CAPS identifiers are not types; plain identifiers stay plain
    assert!(!has("X ID foo", l, "ID", Tok::Type));
    assert!(!has("foo", l, "foo", Tok::Keyword));
    // unterminated string stops before the newline
    assert!(has("\"abc\nint", l, "\"abc", Tok::String));
    assert!(has("\"abc\nint", l, "int", Tok::Keyword));
    // unterminated block comment runs to the end
    assert!(has("a /* open\nstill", l, "/* open\nstill", Tok::Comment));
}

#[test]
fn js_tokens() {
    let src = "const a = `t\nx` + 1.5; function f() { return true } // c";
    let l = Lang::JavaScript;
    assert!(has(src, l, "const", Tok::Keyword));
    assert!(has(src, l, "`t\nx`", Tok::String));
    assert!(has(src, l, "1.5", Tok::Number));
    assert!(has(src, l, "function", Tok::Keyword));
    assert!(has(src, l, "true", Tok::Keyword));
    assert!(has(src, l, "// c", Tok::Comment));
}

#[test]
fn c_family_and_others() {
    assert!(has("#include <x>\nint x;", Lang::Cpp, "#include", Tok::Preproc));
    assert!(has("#include <x>\nint x;", Lang::Cpp, "int", Tok::Keyword));
    assert!(has("fn main() { let x = 'a; }", Lang::Rust, "fn", Tok::Keyword));
    assert!(!toks("<'a>", Lang::Rust).iter().any(|(_, k)| *k == Tok::String), "rust ' is not a string");
    assert!(has("let s: String = \"x\";", Lang::Rust, "String", Tok::Type));
    assert!(has("var s = `raw\nstr`", Lang::Go, "`raw\nstr`", Tok::String));
    assert!(has("x = \"\"\"a\nb\"\"\" # c", Lang::Python, "\"\"\"a\nb\"\"\"", Tok::String));
    assert!(has("x = 1 # c", Lang::Python, "# c", Tok::Comment));
    assert!(has("def f(): pass", Lang::Python, "def", Tok::Keyword));
    assert!(has("echo $HOME # c", Lang::Shell, "$HOME", Tok::Attr));
    assert!(has("echo $HOME # c", Lang::Shell, "# c", Tok::Comment));
    assert!(has("FUNCTION f { }", Lang::PowerShell, "FUNCTION", Tok::Keyword));
    assert!(has("<# a\nb #> $x", Lang::PowerShell, "<# a\nb #>", Tok::Comment));
    assert!(has("$x = 1", Lang::PowerShell, "$x", Tok::Attr));
    assert!(has("select * From t where a = 'x' -- c", Lang::Sql, "select", Tok::Keyword));
    assert!(has("select * From t where a = 'x' -- c", Lang::Sql, "From", Tok::Keyword));
    assert!(has("select * From t where a = 'x' -- c", Lang::Sql, "'x'", Tok::String));
    assert!(has("select * From t where a = 'x' -- c", Lang::Sql, "-- c", Tok::Comment));
    // batch: REM only at the start of a line, :: anywhere
    assert!(has("REM hello\nset x=1", Lang::Batch, "REM hello", Tok::Comment));
    assert!(has("set x=1\n:: c", Lang::Batch, ":: c", Tok::Comment));
    assert!(!has("echo REM x", Lang::Batch, "REM x", Tok::Comment));
    assert!(has("ECHO off", Lang::Batch, "ECHO", Tok::Keyword));
    assert!(has("class A extends B {}", Lang::Java, "extends", Tok::Keyword));
    assert!(has("fun x() = 1", Lang::Java, "fun", Tok::Keyword));
}

#[test]
fn json_keys_vs_values() {
    let src = "{\"name\" : \"v\", \"n\": 12, \"t\": true, \"a\": [null, -1.5e3]}";
    let l = Lang::Json;
    assert!(has(src, l, "\"name\"", Tok::Attr));
    assert!(has(src, l, "\"n\"", Tok::Attr));
    assert!(has(src, l, "\"v\"", Tok::String));
    assert!(has(src, l, "12", Tok::Number));
    assert!(has(src, l, "true", Tok::Keyword));
    assert!(has(src, l, "null", Tok::Keyword));
    assert!(has(src, l, "1.5e3", Tok::Number));
}

#[test]
fn xml_tokens() {
    let src = "<?xml version=\"1.0\"?>\n<!-- c --><Root xmlns:x=\"u\" a='b'>t &amp; u<![CDATA[raw]]><Item/></Root>";
    let l = Lang::Xml;
    assert!(has(src, l, "<?xml version=\"1.0\"?>", Tok::Preproc));
    assert!(has(src, l, "<!-- c -->", Tok::Comment));
    assert!(has(src, l, "<Root", Tok::Tag));
    assert!(has(src, l, "xmlns:x", Tok::Attr));
    assert!(has(src, l, "\"u\"", Tok::AttrValue));
    assert!(has(src, l, "'b'", Tok::AttrValue));
    assert!(has(src, l, "&amp;", Tok::Number));
    assert!(has(src, l, "<![CDATA[raw]]>", Tok::String));
    assert!(has(src, l, "<Item/></Root>", Tok::Tag));
    // XML never treats script bodies as code
    assert!(!has("<script>var x = 1;</script>", l, "var", Tok::Keyword));
}

#[test]
fn html_embedding() {
    let src = "<html><SCRIPT type=\"x\">var a = 1; // c\n</SCRIPT><style>p { color: #fff; margin: 0 }</style><script src=a.js /><b>var</b></html>";
    let l = Lang::Html;
    assert!(has(src, l, "var", Tok::Keyword), "script body should be JS");
    assert!(has(src, l, "// c", Tok::Comment));
    assert!(has(src, l, "1", Tok::Number));
    assert!(has(src, l, "p", Tok::Type), "style body should be CSS (selector)");
    assert!(has(src, l, "color", Tok::Attr));
    assert!(has(src, l, "#fff", Tok::Number));
    // the text of an ordinary element is not code, and a self-closing <script/> has no body
    let k: Vec<_> = toks(src, l).into_iter().filter(|(t, k)| *t == "var" && *k == Tok::Keyword).collect();
    assert_eq!(k.len(), 1);
    // unterminated script runs to the end
    assert!(has("<script>if (x) {", l, "if", Tok::Keyword));
}

#[test]
fn css_tokens() {
    let src = "@media screen { .a:hover, #id { color: #1a2b3c; margin: 10px .5em; content: \"x\"; } } /* c */";
    let l = Lang::Css;
    assert!(has(src, l, "@media", Tok::Keyword));
    assert!(has(src, l, "screen", Tok::Plain) || !has(src, l, "screen", Tok::Keyword));
    assert!(has(src, l, "color", Tok::Attr));
    assert!(has(src, l, "#1a2b3c", Tok::Number));
    assert!(has(src, l, "10px", Tok::Number));
    assert!(has(src, l, ".5em", Tok::Number));
    assert!(has(src, l, "\"x\"", Tok::String));
    assert!(has(src, l, "/* c */", Tok::Comment));
}

#[test]
fn key_value_tokens() {
    let src = "[section]\nkey = \"str\" # trailing\nport: 8080\nflag: yes\n# c\n; d\n- item\nname: plain value";
    let l = Lang::KeyValue;
    assert!(has(src, l, "[section]", Tok::Type));
    assert!(has(src, l, "key", Tok::Attr));
    assert!(has(src, l, "\"str\"", Tok::String));
    assert!(has(src, l, "# trailing", Tok::Comment));
    assert!(has(src, l, "port", Tok::Attr));
    assert!(has(src, l, "8080", Tok::Number));
    assert!(has(src, l, "yes", Tok::Keyword));
    assert!(has(src, l, "# c", Tok::Comment));
    assert!(has(src, l, "; d", Tok::Comment));
    assert!(has(src, l, "-", Tok::Keyword));
    assert!(!has(src, l, "plain value", Tok::String));
}

#[test]
fn markdown_tokens() {
    let src = "# Title\ntext `inline` more\n- item\n1. one\n> quote\n```rust\nlet x = 1;\n```\nafter";
    let l = Lang::Markdown;
    assert!(has(src, l, "# Title", Tok::Heading));
    assert!(has(src, l, "`inline`", Tok::String));
    assert!(has(src, l, "-", Tok::Keyword));
    assert!(has(src, l, "1.", Tok::Keyword));
    assert!(has(src, l, "> quote", Tok::Comment));
    assert!(has(src, l, "```rust", Tok::Comment));
    assert!(has(src, l, "let x = 1;", Tok::String));
    assert!(!toks(src, l).iter().any(|(t, _)| t.contains("after")));
}

#[test]
fn language_mapping() {
    let table: &[(&str, Option<Lang>)] = &[
        ("a.cs", Some(Lang::CSharp)),
        ("a.CSX", Some(Lang::CSharp)),
        ("a.js", Some(Lang::JavaScript)),
        ("a.mjs", Some(Lang::JavaScript)),
        ("a.cjs", Some(Lang::JavaScript)),
        ("a.jsx", Some(Lang::JavaScript)),
        ("a.ts", Some(Lang::JavaScript)),
        ("a.TSX", Some(Lang::JavaScript)),
        ("a.java", Some(Lang::Java)),
        ("a.kt", Some(Lang::Java)),
        ("a.kts", Some(Lang::Java)),
        ("a.c", Some(Lang::Cpp)),
        ("a.h", Some(Lang::Cpp)),
        ("a.cpp", Some(Lang::Cpp)),
        ("a.hpp", Some(Lang::Cpp)),
        ("a.cc", Some(Lang::Cpp)),
        ("a.cxx", Some(Lang::Cpp)),
        ("a.hh", Some(Lang::Cpp)),
        ("a.go", Some(Lang::Go)),
        ("a.rs", Some(Lang::Rust)),
        ("a.py", Some(Lang::Python)),
        ("a.pyw", Some(Lang::Python)),
        ("a.sh", Some(Lang::Shell)),
        ("a.bash", Some(Lang::Shell)),
        ("a.zsh", Some(Lang::Shell)),
        ("a.ps1", Some(Lang::PowerShell)),
        ("a.psm1", Some(Lang::PowerShell)),
        ("a.psd1", Some(Lang::PowerShell)),
        ("a.sql", Some(Lang::Sql)),
        ("a.bat", Some(Lang::Batch)),
        ("a.CMD", Some(Lang::Batch)),
        ("a.json", Some(Lang::Json)),
        ("a.jsonc", Some(Lang::Json)),
        ("a.xml", Some(Lang::Xml)),
        ("a.xaml", Some(Lang::Xml)),
        ("App.csproj", Some(Lang::Xml)),
        ("a.vbproj", Some(Lang::Xml)),
        ("a.fsproj", Some(Lang::Xml)),
        ("Directory.Build.props", Some(Lang::Xml)),
        ("a.targets", Some(Lang::Xml)),
        ("web.config", Some(Lang::Xml)),
        ("a.svg", Some(Lang::Xml)),
        ("a.resx", Some(Lang::Xml)),
        ("a.nuspec", Some(Lang::Xml)),
        ("a.manifest", Some(Lang::Xml)),
        ("a.wxs", Some(Lang::Xml)),
        ("a.xsd", Some(Lang::Xml)),
        ("a.xslt", Some(Lang::Xml)),
        ("a.plist", Some(Lang::Xml)),
        ("a.vsixmanifest", Some(Lang::Xml)),
        ("a.slnx", Some(Lang::Xml)),
        ("a.html", Some(Lang::Html)),
        ("a.htm", Some(Lang::Html)),
        ("a.xhtml", Some(Lang::Html)),
        ("a.css", Some(Lang::Css)),
        ("a.scss", Some(Lang::Css)),
        ("a.less", Some(Lang::Css)),
        ("a.yaml", Some(Lang::KeyValue)),
        ("a.yml", Some(Lang::KeyValue)),
        ("a.toml", Some(Lang::KeyValue)),
        ("a.ini", Some(Lang::KeyValue)),
        ("a.cfg", Some(Lang::KeyValue)),
        ("a.conf", Some(Lang::KeyValue)),
        ("a.properties", Some(Lang::KeyValue)),
        (".editorconfig", Some(Lang::KeyValue)),
        ("a.md", Some(Lang::Markdown)),
        ("README.MARKDOWN", Some(Lang::Markdown)),
        ("C:\\dir.x\\sub\\file.RS", Some(Lang::Rust)),
        ("dir.cs/file", None),
        ("a.txt", None),
        ("a.log", None),
        ("a.csv", None),
        ("a.exe", None),
        ("Makefile", None),
        ("a.", None),
        ("", None),
        ("a.verylongextensionname", None),
        ("ü.rs", Some(Lang::Rust)),
        ("a.ü", None),
    ];
    for (name, want) in table {
        assert_eq!(language_for(name), *want, "language_for({name:?})");
    }
}

#[test]
fn palette() {
    assert_eq!(color(Tok::Comment, false), 0x008000);
    assert_eq!(color(Tok::Comment, true), 0x6A9955);
    assert_eq!(color(Tok::String, false), 0xA31515);
    assert_eq!(color(Tok::String, true), 0xCE9178);
    assert_eq!(color(Tok::Number, false), 0x098658);
    assert_eq!(color(Tok::Number, true), 0xB5CEA8);
    assert_eq!(color(Tok::Keyword, false), 0x0000FF);
    assert_eq!(color(Tok::Keyword, true), 0x569CD6);
    assert_eq!(color(Tok::Type, false), 0x267F99);
    assert_eq!(color(Tok::Type, true), 0x4EC9B0);
    assert_eq!(color(Tok::Tag, false), 0x800000);
    assert_eq!(color(Tok::Tag, true), 0x569CD6);
    assert_eq!(color(Tok::Attr, false), 0xE50000);
    assert_eq!(color(Tok::Attr, true), 0x9CDCFE);
    assert_eq!(color(Tok::AttrValue, false), 0x0000FF);
    assert_eq!(color(Tok::AttrValue, true), 0xCE9178);
    assert_eq!(color(Tok::Preproc, false), 0xAF00DB);
    assert_eq!(color(Tok::Preproc, true), 0xC586C0);
    assert_eq!(color(Tok::Heading, false), 0x0000FF);
    assert_eq!(color(Tok::Heading, true), 0x569CD6);
    assert!(is_bold(Tok::Heading) && !is_bold(Tok::Keyword));
}

#[test]
fn speed_two_megabytes() {
    let cs_block = "using System;\n// comment ü\nnamespace Demo { public class Foo { private string s = \"hello \\\" world\"; public int Bar(int x) { return x * 42 + 3; } /* block */ } }\n";
    let json_block = "{\"name\": \"value ü\", \"n\": 12345.5e3, \"flag\": true, \"list\": [1, 2, 3, null], \"nested\": {\"k\": \"v\"}}\n";
    for (lang, block) in [(Lang::CSharp, cs_block), (Lang::Json, json_block)] {
        let text = block.repeat(2_000_000 / block.len() + 1);
        let t0 = Instant::now();
        let spans = tokenize(&text, lang);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        println!("{lang:?}: {} bytes, {} spans in {ms:.1} ms", text.len(), spans.len());
        assert!(ms < 400.0, "{lang:?} took {ms:.1} ms");
        assert_eq!(spans.iter().map(|s| s.len).sum::<usize>(), text.len());
    }
}
