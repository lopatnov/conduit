;; header-injector.wat — WASM request-phase middleware
;;
;; No toolchain needed — Conduit's integration tests compile this automatically
;; using the bundled `wat` crate.  For a standalone .wasm:
;;   wasm-tools parse header-injector.wat -o header-injector.wasm
;;
;; Request phase (on_request):
;;   - Copies X-Request-ID → X-Trace-Id  (upstream correlation header)
;;   - Injects X-Wasm-Plugin: header-injector/1.0  onto the forwarded request
;;   - If X-Debug: 1 is present → logs a message at INFO level

(module
  ;; ── Imports ────────────────────────────────────────────────────────────────
  (import "conduit" "conduit_get_header"
    (func $get_header (param i32 i32 i32 i32) (result i32)))
  (import "conduit" "conduit_set_request_header"
    (func $set_req_header (param i32 i32 i32 i32)))
  (import "conduit" "conduit_log"
    (func $log (param i32 i32 i32)))

  ;; ── Linear memory ──────────────────────────────────────────────────────────
  (memory (export "memory") 1)   ;; 64 KiB

  ;; Static string layout (no overlaps):
  ;; [  0] "x-request-id"           12 bytes
  ;; [ 16] "x-trace-id"             10 bytes
  ;; [ 32] "x-wasm-plugin"          13 bytes
  ;; [ 48] "header-injector/1.0"    19 bytes
  ;; [ 72] "x-debug"                 7 bytes
  ;; [ 80] "1"                       1 byte
  ;; [ 96] "wasm: X-Debug hit\0"    18 bytes
  ;; [256..511] read buffer for header values

  (data (i32.const   0) "x-request-id")
  (data (i32.const  16) "x-trace-id")
  (data (i32.const  32) "x-wasm-plugin")
  (data (i32.const  48) "header-injector/1.0")
  (data (i32.const  72) "x-debug")
  (data (i32.const  80) "1")
  (data (i32.const  96) "wasm: X-Debug hit")

  ;; ── on_request ─────────────────────────────────────────────────────────────
  (func (export "on_request") (result i32)
    (local $len i32)

    ;; 1. Read X-Request-ID into buf[256]
    i32.const 0    ;; "x-request-id"  ptr
    i32.const 12   ;; name length
    i32.const 256  ;; output buffer
    i32.const 256  ;; buffer capacity
    call $get_header
    local.set $len

    ;; 2. If present (len > 0) → forward as X-Trace-Id to upstream
    (block $no_trace
      local.get $len
      i32.const 0
      i32.le_s
      br_if $no_trace
      i32.const 16   ;; "x-trace-id"
      i32.const 10
      i32.const 256  ;; value (same buffer)
      local.get $len
      call $set_req_header
    )

    ;; 3. Always inject X-Wasm-Plugin: header-injector/1.0
    i32.const 32   ;; "x-wasm-plugin"
    i32.const 13
    i32.const 48   ;; "header-injector/1.0"
    i32.const 19
    call $set_req_header

    ;; 4. If X-Debug: 1 → emit an INFO log entry
    i32.const 72   ;; "x-debug"
    i32.const 7
    i32.const 256  ;; reuse buffer
    i32.const 2    ;; capacity = 2 (only need 1 byte)
    call $get_header
    local.set $len
    (block $no_log
      local.get $len
      i32.const 1
      i32.ne
      br_if $no_log
      ;; Verify the byte is ASCII '1' (49)
      i32.const 256
      i32.load8_u
      i32.const 49
      i32.ne
      br_if $no_log
      ;; log(level=2 INFO, msg_ptr, msg_len)
      i32.const 2
      i32.const 96   ;; "wasm: X-Debug hit"
      i32.const 17
      call $log
    )

    i32.const 0  ;; Continue (do not abort)
  )
)
