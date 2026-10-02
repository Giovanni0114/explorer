use std::{
    fs,
    io::Read,
    path::Path,
    sync::Arc,
};

use hayro::{RenderSettings, hayro_interpret::InterpreterSettings, hayro_syntax::Pdf};
use image::{DynamicImage, RgbaImage};

use crate::imageview::ImageData;

pub(crate) struct RenderedPdf {
    pub image: ImageData,
    pub page_count: usize,
    pub page_size_points: (u32, u32),
}

pub(crate) fn render_first_page(
    path: &Path,
    target: (u32, u32),
) -> Result<RenderedPdf, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(crate::preview::MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > crate::preview::MAX_IMAGE_BYTES {
        return Err("PDF is too large to preview".into());
    }
    let pdf = Pdf::new(Arc::new(bytes)).map_err(|e| format!("{e:?}"))?;
    let pages = pdf.pages();
    let page = pages.first().ok_or("the PDF has no pages")?;
    let (width, height) = page.render_dimensions();
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("the first page has invalid dimensions".into());
    }
    let scale = (target.0 as f32 / width)
        .min(target.1 as f32 / height)
        .min(1.0);
    if !scale.is_finite() || scale <= 0.0 {
        return Err("the preview target is invalid".into());
    }
    let scaled = |dimension: f32| {
        (dimension * scale)
            .floor()
            .clamp(1.0, u16::MAX as f32) as u16
    };
    let pixmap = hayro::render(
        page,
        &InterpreterSettings::default(),
        &RenderSettings {
            x_scale: scale,
            y_scale: scale,
            width: Some(scaled(width)),
            height: Some(scaled(height)),
            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
            ..Default::default()
        },
    );
    let (image_width, image_height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
    let pixels = pixmap
        .take_unpremultiplied()
        .into_iter()
        .flat_map(|pixel| [pixel.r, pixel.g, pixel.b, pixel.a])
        .collect();
    let image = RgbaImage::from_raw(image_width, image_height, pixels)
        .ok_or("the PDF renderer returned an invalid image")?;
    Ok(RenderedPdf {
        image: ImageData(Arc::new(DynamicImage::ImageRgba8(image))),
        page_count: pages.len(),
        page_size_points: (width.round() as u32, height.round() as u32),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    fn document(objects: &[String], trailer: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (number, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", number + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R {trailer} >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn stream(contents: &str) -> String {
        format!(
            "<< /Length {} >>\nstream\n{contents}\nendstream",
            contents.len()
        )
    }

    fn two_page_pdf() -> Vec<u8> {
        document(
            &[
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 50] /Contents 5 0 R >>".into(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 50] /Contents 6 0 R >>".into(),
                stream("1 0 0 rg 0 0 100 50 re f"),
                stream("0 0 1 rg 0 0 100 50 re f"),
            ],
            "",
        )
    }

    fn one_page_pdf(width: u32, height: u32) -> Vec<u8> {
        document(
            &[
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Contents 4 0 R >>"
                ),
                stream("1 0 0 rg 0 0 1 1 re f"),
            ],
            "",
        )
    }

    fn zero_page_pdf() -> Vec<u8> {
        document(
            &[
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Kids [] /Count 0 >>".into(),
            ],
            "",
        )
    }

    fn encrypted_pdf() -> Vec<u8> {
        document(
            &[
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 50] >>".into(),
                "<< /Filter /Standard /V 1 /R 2 /Length 40 /O <0000000000000000000000000000000000000000000000000000000000000000> /U <0000000000000000000000000000000000000000000000000000000000000000> /P -4 >>".into(),
            ],
            "/Encrypt 4 0 R",
        )
    }

    fn file(bytes: &[u8]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), bytes).unwrap();
        file
    }

    #[test]
    fn renders_only_the_first_page_and_reports_document_metadata() {
        let file = file(&two_page_pdf());
        let rendered = super::render_first_page(file.path(), (100, 50)).unwrap();
        assert_eq!(rendered.page_count, 2);
        assert_eq!(rendered.page_size_points, (100, 50));
        let pixel = rendered.image.0.to_rgb8().get_pixel(50, 25).0;
        assert!(pixel[0] > pixel[2], "page one should be red: {pixel:?}");
    }

    #[test]
    fn scales_first_page_to_the_target_without_distortion_or_upscaling() {
        let file = file(&two_page_pdf());
        let shrunk = super::render_first_page(file.path(), (40, 40)).unwrap();
        assert_eq!((shrunk.image.0.width(), shrunk.image.0.height()), (40, 20));
        let natural = super::render_first_page(file.path(), (500, 500)).unwrap();
        assert_eq!((natural.image.0.width(), natural.image.0.height()), (100, 50));
    }

    #[test]
    fn extreme_page_ratios_keep_both_raster_dimensions_positive() {
        let file = file(&one_page_pdf(100_000, 1));
        let rendered = super::render_first_page(file.path(), (64, 64)).unwrap();
        assert_eq!(rendered.image.0.width(), 64);
        assert!(rendered.image.0.height() >= 1);
    }

    #[test]
    fn malformed_and_empty_pdfs_return_errors() {
        let malformed = file(b"%PDF-1.4\nnot a document");
        assert!(super::render_first_page(malformed.path(), (100, 50)).is_err());
        let empty = file(&zero_page_pdf());
        assert!(super::render_first_page(empty.path(), (100, 50)).is_err());
    }

    #[test]
    fn encrypted_pdfs_return_an_error() {
        let file = file(&encrypted_pdf());
        assert!(super::render_first_page(file.path(), (100, 50)).is_err());
    }

    #[test]
    fn refuses_oversize_documents_before_parsing() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), b"%PDF-1.4\n").unwrap();
        file.as_file().set_len(crate::preview::MAX_IMAGE_BYTES + 1).unwrap();
        match super::render_first_page(file.path(), (100, 50)) {
            Err(error) => assert_eq!(error, "PDF is too large to preview"),
            Ok(_) => panic!("oversize PDF was rendered"),
        }
    }
}
