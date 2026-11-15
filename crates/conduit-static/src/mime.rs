//! Extension-based Content-Type detection.
//!
//! Moved from `crates/conduit-core/src/util/mime.rs` (issue #114/#139) —
//! this crate's `src/handler.rs` was its only caller (via
//! `src/util/mime.rs`'s root-crate facade). See this crate's `src/lib.rs`
//! doc comment, "What moved out of `conduit-core`".

use std::path::Path;

/// The media types of adaptive streaming, which `mime_guess` gets wrong or does not know. HLS (RFC 8216 section 4) names
/// `application/vnd.apple.mpegurl` or `audio/mpegurl` for a playlist and the MPEG-2 transport stream type is `video/mp2t`
/// (IANA); a player or a CDN that goes by the header, not the extension, needs the right one. nginx's `mime.types` and
/// Caddy's table carry the same values.
fn streaming_type(extension: &str) -> Option<&'static str> {
    match extension {
        "m3u8" => Some("application/vnd.apple.mpegurl"),
        "m3u" => Some("audio/mpegurl"),
        "ts" => Some("video/mp2t"),
        "m4s" => Some("video/iso.segment"),
        "mpd" => Some("application/dash+xml"),
        _ => None,
    }
}

pub fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if let Some(known) = extension.as_deref().and_then(streaming_type) {
        return known;
    }
    mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HLS and DASH: the types a player or a CDN looks at (RFC 8216 section 4 for playlists, IANA for the segments).
    #[test]
    fn streaming_media_extensions() {
        for (name, expected) in [
            ("index.m3u8", "application/vnd.apple.mpegurl"),
            ("INDEX.M3U8", "application/vnd.apple.mpegurl"),
            ("list.m3u", "audio/mpegurl"),
            ("seg0.ts", "video/mp2t"),
            ("SEG0.TS", "video/mp2t"),
            ("init.mp4", "video/mp4"),
            ("chunk.m4s", "video/iso.segment"),
            ("manifest.mpd", "application/dash+xml"),
        ] {
            assert_eq!(content_type(Path::new(name)), expected, "{name}");
        }
        // a directory called like a segment is not special, and a name without an extension still falls back
        assert_eq!(
            content_type(Path::new("video.ts/readme")),
            "application/octet-stream"
        );
    }

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
