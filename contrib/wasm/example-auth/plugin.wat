;; Example Conduit WASM plugin: API key auth + X-Tenant redirect
;;
;; This plugin demonstrates the V2 WASM ABI:
;;   - Reads a custom header (X-API-Key)
;;   - If key matches, injects X-Tenant-ID and continues
;;   - If missing, aborts with 401 Unauthorized
;;   - If key == "redirect", issues a 302 redirect
;;
;; Build:   wat2wasm plugin.wat -o plugin.wasm
;; Or with wabt: wat2wasm plugin.wat
;;
;; Config (conduit.yaml):
;;   middleware:
;;     - type: wasm
;;       path: ./contrib/wasm/example-auth/plugin.wasm
;;       config: { "validKey": "secret-token-123", "tenant": "acme" }

(module
    ;; ── Imports ───────────────────────────────────────────────────────────────
    (import "conduit" "conduit_get_header"
        (func $get_header (param i32 i32 i32 i32) (result i32)))
    (import "conduit" "conduit_get_plugin_config"
        (func $get_config (param i32 i32) (result i32)))
    (import "conduit" "conduit_set_request_header"
        (func $set_req_hdr (param i32 i32 i32 i32)))
    (import "conduit" "conduit_set_response_status"
        (func $set_status (param i32)))
    (import "conduit" "conduit_set_response_body"
        (func $set_body (param i32 i32)))
    (import "conduit" "conduit_abort_with_redirect"
        (func $redirect (param i32 i32)))
    (import "conduit" "conduit_log"
        (func $log (param i32 i32 i32)))

    ;; ── Memory layout ─────────────────────────────────────────────────────────
    (memory (export "memory") 1)

    ;; Static strings
    (data (i32.const  0) "x-api-key")           ;; len  9, offset  0
    (data (i32.const 10) "x-tenant-id")         ;; len 11, offset 10
    (data (i32.const 22) "unauthorized")         ;; len 12, offset 22
    (data (i32.const 35) "https://login.example.com/")  ;; len 26, offset 35
    (data (i32.const 62) "redirect")             ;; len  8, offset 62 (special key)
    (data (i32.const 71) "WASM: valid key, continuing") ;; len 27, debug msg

    ;; Dynamic buffers
    ;; [200..400) = incoming X-API-Key value
    ;; [400..800) = plugin config JSON

    ;; ── on_request ────────────────────────────────────────────────────────────
    (func (export "on_request") (result i32)
        (local $key_len i32)

        ;; 1. Read X-API-Key header into buf[200..400)
        (local.set $key_len
            (call $get_header
                (i32.const 0)   (i32.const 9)    ;; "x-api-key"
                (i32.const 200) (i32.const 200))) ;; buffer

        ;; 2. If header absent (key_len == -1), deny with 401
        (if (i32.eq (local.get $key_len) (i32.const -1))
            (then
                (call $set_status (i32.const 401))
                (call $set_body (i32.const 22) (i32.const 12)) ;; "unauthorized"
                (return (i32.const 1))))

        ;; 3. If key == "redirect" (8 bytes), issue 302
        ;; (Demo only: checks length==8 and first 4 bytes == "redi" via i32.load;
        ;;  a production plugin should compare all 8 bytes with two i32.load checks.)
        (if (i32.and
                (i32.eq (local.get $key_len) (i32.const 8))
                (i32.eq
                    (i32.load (i32.const 200))
                    (i32.load (i32.const 62))))
            (then
                (call $redirect (i32.const 35) (i32.const 26))
                (return (i32.const 1))))

        ;; 4. Valid key — inject X-Tenant-ID and log
        ;; In a real plugin you'd compare the key against config; here we
        ;; accept any non-empty key for demonstration purposes.
        (call $set_req_hdr
            (i32.const 10) (i32.const 11)  ;; "x-tenant-id"
            (i32.const 200) (local.get $key_len)) ;; echo key as tenant (demo only!)
        (call $log (i32.const 2) (i32.const 71) (i32.const 27)) ;; info

        i32.const 0  ;; Continue
    )
)
