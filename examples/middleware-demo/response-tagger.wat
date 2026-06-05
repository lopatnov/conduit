;; response-tagger.wat — WASM response-phase middleware
;;
;; Adds X-Processed-By: wasm to every response coming from upstream.
;; Works alongside header-injector.wat (request phase) in a pipeline.
;;
;; No toolchain needed — Conduit's integration tests compile this automatically.

(module
  (import "conduit" "conduit_set_response_header"
    (func $set_hdr (param i32 i32 i32 i32)))

  (memory (export "memory") 1)

  ;; [0..13]  "x-processed-by"  14 bytes
  ;; [16..19] "wasm"             4 bytes

  (data (i32.const  0) "x-processed-by")
  (data (i32.const 16) "wasm")

  (func (export "on_response") (param $status i32) (result i32)
    i32.const 0   ;; name ptr  "x-processed-by"
    i32.const 14  ;; name len
    i32.const 16  ;; val ptr   "wasm"
    i32.const 4   ;; val len
    call $set_hdr
    i32.const 0   ;; Continue
  )
)
