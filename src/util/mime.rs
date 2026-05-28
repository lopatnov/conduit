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
}
