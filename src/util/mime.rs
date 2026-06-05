use std::path::Path;

pub fn content_type(path: &Path) -> &'static str {
    mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_extension() {
        assert_eq!(content_type(Path::new("index.html")), "text/html");
    }

    #[test]
    fn css_extension() {
        assert_eq!(content_type(Path::new("style.css")), "text/css");
    }

    #[test]
    fn js_extension() {
        // mime_guess may return application/javascript or text/javascript
        let ct = content_type(Path::new("app.js"));
        assert!(ct.contains("javascript"), "unexpected: {ct}");
    }

    #[test]
    fn json_extension() {
        assert_eq!(content_type(Path::new("data.json")), "application/json");
    }

    #[test]
    fn png_extension() {
        assert_eq!(content_type(Path::new("logo.png")), "image/png");
    }

    #[test]
    fn svg_extension() {
        assert_eq!(content_type(Path::new("icon.svg")), "image/svg+xml");
    }

    #[test]
    fn unknown_extension_falls_back_to_octet_stream() {
        assert_eq!(
            content_type(Path::new("data.xyzabc123")),
            "application/octet-stream"
        );
    }

    #[test]
    fn no_extension_falls_back_to_octet_stream() {
        assert_eq!(
            content_type(Path::new("Makefile")),
            "application/octet-stream"
        );
    }

    #[test]
    fn wasm_extension() {
        assert_eq!(content_type(Path::new("plugin.wasm")), "application/wasm");
    }

    #[test]
    fn webp_extension() {
        let ct = content_type(Path::new("image.webp"));
        assert!(ct.contains("webp"), "unexpected: {ct}");
    }

    #[test]
    fn pdf_extension() {
        assert_eq!(content_type(Path::new("doc.pdf")), "application/pdf");
    }

    #[test]
    fn gz_extension() {
        let ct = content_type(Path::new("archive.tar.gz"));
        assert!(
            ct.contains("gzip") || ct.contains("octet"),
            "unexpected: {ct}"
        );
    }

    #[test]
    fn xml_extension() {
        let ct = content_type(Path::new("feed.xml"));
        assert!(ct.contains("xml"), "unexpected: {ct}");
    }

    #[test]
    fn ttf_font_extension() {
        let ct = content_type(Path::new("font.ttf"));
        assert!(!ct.is_empty(), "should not be empty");
    }

    #[test]
    fn path_with_directories_uses_filename_extension() {
        let ct_deep = content_type(Path::new("/some/path/to/style.css"));
        let ct_shallow = content_type(Path::new("style.css"));
        assert_eq!(
            ct_deep, ct_shallow,
            "directory path should not affect mime type"
        );
    }
}
