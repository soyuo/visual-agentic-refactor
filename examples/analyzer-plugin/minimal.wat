(module
  (memory (export "memory") 1)
  (func (export "alloc") (param i32) (result i32)
    i32.const 0)
  (func (export "analyze") (param i32 i32) (result i64)
    i64.const 68719476844)
  (data (i32.const 16)
    "{\"language\":\"kotlin\",\"completeness\":\"partial\",\"symbols\":[],\"imports\":[],\"relationships\":[],\"diagnostics\":[]}")
)
