;; Source for hello.wasm. Regenerate with `wasm-tools parse hello.wat -o hello.wasm`
;; and update SHA256 in tests/test_wasm.py. The module imports host.log, exports
;; memory and run, writes "hello from wasm", then returns 0.
(module
  (import "host" "log" (func $host_log (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello from wasm")
  (func (export "run") (result i32)
    i32.const 0
    i32.const 15
    call $host_log
    i32.const 0))
