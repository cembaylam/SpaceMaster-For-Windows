$f = "src/viewer.rs"
$t = [IO.File]::ReadAllText($f)
$t += @'

impl Viewer {
    /// (pages, pages rendered so far, first visible page) of a PDF view; used by the self-test.
    pub fn pdf_info(&self) -> Option<(usize, usize, usize)> {
        match &self.content {
            Content::Pdf(p) => Some((p.sizes.len(), p.pages.values().filter(|b| b.bmp.is_some()).count(), p.cur_page)),
            _ => None,
        }
    }
}
'@
[IO.File]::WriteAllText($f, $t)

$f = "src/app/selftest.rs"
$t = [IO.File]::ReadAllText($f)
$marker = "    // 11. Final paint"
$add = @'
    // 10e. PDF viewer: a generated two-page PDF renders and scrolls.
    if let Ok(dir) = std::env::var("SM_START_PATH") {
        let content = "BT /F1 72 Tf 72 600 Td (HELLO) Tj ET";
        let objs: Vec<String> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>".into(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for off in offsets {
            pdf.extend(format!("{off:010} 00000 n \n").bytes());
        }
        pdf.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
        let path = format!("{dir}\\viewer-doc.pdf");
        let _ = std::fs::write(&path, &pdf);
        if a().browser.cd(&dir, None, true) {
            a().browser.app_activated();
            let idx = a().browser.current.borrow().index_of(Some("viewer-doc.pdf"));
            if let Some(i) = idx {
                a().browser.move_cursor(i as isize);
                chars(hwnd, " ");
                let opened = pump_until(Duration::from_secs(20), || a().viewer.state_name() == "pdf");
                r.check("PDF opens in the viewer", opened, || a().viewer.state_name().into());
                // paint repeatedly: page renders are requested from paint and arrive as messages
                let rendered = pump_until(Duration::from_secs(20), || {
                    paint_now(hwnd);
                    a().viewer.pdf_info().map(|p| p.1 >= 1).unwrap_or(false)
                });
                r.check("PDF page is rendered", rendered, || format!("{:?}", a().viewer.pdf_info()));
                r.check("PDF reports its page count", a().viewer.pdf_info().map(|p| p.0) == Some(2), || format!("{:?}", a().viewer.pdf_info()));
                chars(hwnd, "G");
                paint_now(hwnd);
                r.check("G scrolls a PDF to the last page", a().viewer.pdf_info().map(|p| p.2) == Some(1), || format!("{:?}", a().viewer.pdf_info()));
                chars(hwnd, "g");
                paint_now(hwnd);
                r.check("g returns to the first page", a().viewer.pdf_info().map(|p| p.2) == Some(0), || format!("{:?}", a().viewer.pdf_info()));
                chars(hwnd, "+");
                chars(hwnd, "-");
                chars(hwnd, "0");
                paint_now(hwnd);
                chars(hwnd, "q");
                r.check("PDF viewer closes", !a().viewer.active, || "still active".into());
            } else {
                r.check("PDF file listed", false, || "missing".into());
            }
        }
        let _ = std::fs::remove_file(&path);
    }

'@
$t = $t.Replace($marker, $add + $marker)
[IO.File]::WriteAllText($f, $t)
"ok"
