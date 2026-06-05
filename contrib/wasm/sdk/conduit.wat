;; Conduit WASM Plugin SDK — ABI stub declarations
;;
;; Copy this file into your WAT plugin or use the signatures to generate
;; bindings in your preferred language (Rust, Go, C, AssemblyScript, etc.).
;;
;; All data passes through WASM linear memory.  Plugins must export "memory".
;; Strings are UTF-8; lengths are byte counts (not character counts).

(module
    ;; ── Request read ──────────────────────────────────────────────────────────

    ;; Write HTTP method into buf[0..buf_len]. Returns bytes written.
    (import "conduit" "conduit_get_method"
        (func (param i32 i32) (result i32)))

    ;; Write request path (without query string) into buf. Returns bytes written.
    (import "conduit" "conduit_get_path"
        (func (param i32 i32) (result i32)))

    ;; Write raw query string (the part after "?") into buf. Empty if no query.
    (import "conduit" "conduit_get_query"
        (func (param i32 i32) (result i32)))

    ;; Write full URI (path + "?" + query when query is non-empty) into buf.
    ;; Example: "/search?q=hello"
    (import "conduit" "conduit_get_uri"
        (func (param i32 i32) (result i32)))

    ;; Write remote client IP address (IPv4 or IPv6) into buf.
    (import "conduit" "conduit_get_client_ip"
        (func (param i32 i32) (result i32)))

    ;; Write the X-Request-ID header value into buf.  Empty string if not set.
    (import "conduit" "conduit_get_request_id"
        (func (param i32 i32) (result i32)))

    ;; Write the value of a named request header into buf.
    ;; name_ptr/name_len: header name (case-insensitive).
    ;; Returns bytes written, or -1 if the header is absent.
    (import "conduit" "conduit_get_header"
        (func (param i32 i32 i32 i32) (result i32)))

    ;; Returns the number of request headers.
    (import "conduit" "conduit_get_header_count"
        (func (result i32)))

    ;; Write all header names as a newline-separated list into buf.
    ;; Example: "content-type\nauthorization\nx-custom"
    ;; Returns total bytes written (may be truncated at buf_len).
    (import "conduit" "conduit_get_header_names"
        (func (param i32 i32) (result i32)))

    ;; Write the JSON config from MiddlewareEntry.config into buf.
    ;; Empty if no config was specified in conduit.yaml.
    (import "conduit" "conduit_get_plugin_config"
        (func (param i32 i32) (result i32)))

    ;; ── Request mutation ──────────────────────────────────────────────────────

    ;; Add or overwrite a request header.
    ;; name_ptr/name_len: header name.  val_ptr/val_len: new value.
    (import "conduit" "conduit_set_request_header"
        (func (param i32 i32 i32 i32)))

    ;; Remove a request header by name (case-insensitive).
    (import "conduit" "conduit_remove_request_header"
        (func (param i32 i32)))

    ;; ── Response control (abort path) ─────────────────────────────────────────
    ;; Call these BEFORE returning 1 from on_request() to customise the
    ;; rejection response.

    ;; Set the HTTP status code (100–999) for the abort response. Default: 500.
    (import "conduit" "conduit_set_response_status"
        (func (param i32)))

    ;; Add a header to the abort response.
    (import "conduit" "conduit_set_response_header"
        (func (param i32 i32 i32 i32)))

    ;; Set the body of the abort response (replaces any previous call).
    (import "conduit" "conduit_set_response_body"
        (func (param i32 i32)))

    ;; Abort with a 302 Found redirect.
    ;; Equivalent to: set_response_status(302) + set_response_header("location", url)
    ;; url_ptr/url_len: the Location header value.
    (import "conduit" "conduit_abort_with_redirect"
        (func (param i32 i32)))

    ;; ── Logging ───────────────────────────────────────────────────────────────

    ;; Emit a log message at the given level:
    ;;   0 = trace, 1 = debug, 2 = info, 3 = warn, 4 = error
    (import "conduit" "conduit_log"
        (func (param i32 i32 i32)))

    ;; ── Required export ───────────────────────────────────────────────────────

    ;; on_request() → i32
    ;;   0 = Continue (forward to upstream)
    ;;   1 = Abort    (return the configured response to the client)
    ;;
    ;; Plugins must also export "memory".
)
