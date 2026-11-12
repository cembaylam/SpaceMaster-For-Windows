//! PDF rendering with the PDF engine built into Windows (`Windows.Data.Pdf`): no extra dependency.
//! A [`PdfDoc`] must be used from a thread initialised for COM (MTA); calls block, so use a worker thread.

use windows::core::HSTRING;
use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
use windows::Storage::{FileAccessMode, StorageFile};
use windows::UI::Color;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::SHCreateMemStream;

pub struct PdfDoc {
    doc: PdfDocument,
}

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

impl PdfDoc {
    pub fn open(path: &str) -> Result<PdfDoc, String> {
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path)).map_err(e)?.join().map_err(e)?;
        let stream = file.OpenAsync(FileAccessMode::Read).map_err(e)?.join().map_err(e)?;
        let doc = PdfDocument::LoadFromStreamAsync(&stream).map_err(e)?.join().map_err(e)?;
        if doc.IsPasswordProtected().unwrap_or(false) {
            return Err("This PDF is password protected.".into());
        }
        Ok(PdfDoc { doc })
    }

    pub fn page_count(&self) -> usize {
        self.doc.PageCount().unwrap_or(0) as usize
    }

    /// Page size in device-independent units (1/96 inch, i.e. CSS pixels): US Letter is 816 x 1056.
    pub fn page_size(&self, index: usize) -> Option<(f32, f32)> {
        let p = self.doc.GetPage(index as u32).ok()?;
        let s = p.Size().ok()?;
        Some((s.Width, s.Height))
    }

    /// Renders a page at `width_px` pixels wide (white background) as premultiplied BGRA.
    pub fn render(&self, index: usize, width_px: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let page = self.doc.GetPage(index as u32).map_err(e)?;
        let opts = PdfPageRenderOptions::new().map_err(e)?;
        opts.SetDestinationWidth(width_px.max(16)).map_err(e)?;
        opts.SetBackgroundColor(Color { A: 255, R: 255, G: 255, B: 255 }).map_err(e)?;
        let ms = InMemoryRandomAccessStream::new().map_err(e)?;
        page.RenderWithOptionsToStreamAsync(&ms, &opts).map_err(e)?.join().map_err(e)?;
        let size = ms.Size().map_err(e)? as u32;
        let reader = DataReader::CreateDataReader(&ms.GetInputStreamAt(0).map_err(e)?).map_err(e)?;
        reader.LoadAsync(size).map_err(e)?.join().map_err(e)?;
        let mut png = vec![0u8; size as usize];
        reader.ReadBytes(&mut png).map_err(e)?;
        decode_png(&png)
    }
}

fn decode_png(png: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(e)?;
        let stream = SHCreateMemStream(Some(png)).ok_or("no memory stream")?;
        let dec = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).map_err(e)?;
        let frame = dec.GetFrame(0).map_err(e)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).map_err(e)?;
        let conv = wic.CreateFormatConverter().map_err(e)?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .map_err(e)?;
        let mut data = vec![0u8; (w as usize) * (h as usize) * 4];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut data).map_err(e)?;
        Ok((w, h, data))
    }
}
