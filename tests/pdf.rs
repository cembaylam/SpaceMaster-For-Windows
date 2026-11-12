use spacemaster::pdf::PdfDoc;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// A valid two-page PDF (US Letter) with big black text, built with a correct xref table.
fn sample_pdf() -> Vec<u8> {
    let content = "BT /F1 72 Tf 72 600 Td (HELLO) Tj ET";
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Resources << >> >>".into(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
    out
}

#[test]
fn opens_measures_and_renders_pages() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pdf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.pdf");
    std::fs::write(&file, sample_pdf()).unwrap();

    let doc = PdfDoc::open(file.to_str().unwrap()).expect("open");
    assert_eq!(doc.page_count(), 2);
    let (w, h) = doc.page_size(0).unwrap();
    assert!((w - 816.0).abs() < 1.0 && (h - 1056.0).abs() < 1.0, "{w}x{h}");
    let (w2, h2) = doc.page_size(1).unwrap();
    assert!((w2 - 400.0).abs() < 1.0 && (h2 - 533.33).abs() < 1.0);
    assert!(doc.page_size(5).is_none());

    let t = std::time::Instant::now();
    let (rw, rh, data) = doc.render(0, 800).expect("render");
    println!("render 800 px wide: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    assert_eq!(rw, 800);
    assert!((rh as f32 - 800.0 * 792.0 / 612.0).abs() < 3.0, "{rh}");
    assert_eq!(data.len(), (rw * rh * 4) as usize);
    // White paper with black text: both must be present.
    let white = data.chunks_exact(4).filter(|p| p[0] > 240 && p[1] > 240 && p[2] > 240).count();
    let dark = data.chunks_exact(4).filter(|p| p[0] < 60 && p[1] < 60 && p[2] < 60).count();
    assert!(white > data.len() / 8, "page should be mostly white");
    assert!(dark > 500, "text pixels missing: {dark}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rejects_non_pdf_files() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pdf-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("not-a.pdf");
    std::fs::write(&file, b"this is not a pdf").unwrap();
    assert!(PdfDoc::open(file.to_str().unwrap()).is_err());
    assert!(PdfDoc::open(dir.join("missing.pdf").to_str().unwrap()).is_err());
    let _ = std::fs::remove_dir_all(dir);
}
