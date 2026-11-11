(module
  (import "env" "is_cancelled" (func $is_cancelled (result i32)))
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32)
    i32.const 256)
  (func (export "analyze") (param i32 i32) (result i64)
    (call $is_cancelled)
    drop
    i64.const 68719476844)
  (data (i32.const 16)
    "{\"language\":\"kotlin\",\"completeness\":\"partial\",\"symbols\":[],\"imports\":[],\"relationships\":[],\"diagnostics\":[]}")
)
