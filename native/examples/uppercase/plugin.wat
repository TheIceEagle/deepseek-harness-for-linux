;; Native text plugin API v1. No imports, filesystem, network or WASI.
(module
  (memory (export "memory") 2 128)
  (func (export "alloc") (param i32) (result i32)
    i32.const 1024)
  (func (export "transform") (param $pointer i32) (param $length i32) (result i64)
    (local $index i32)
    (local $character i32)
    (block $done
      (loop $next
        local.get $index
        local.get $length
        i32.ge_u
        br_if $done
        local.get $pointer
        local.get $index
        i32.add
        i32.load8_u
        local.tee $character
        i32.const 97
        i32.ge_u
        local.get $character
        i32.const 122
        i32.le_u
        i32.and
        (if
          (then
            local.get $pointer
            local.get $index
            i32.add
            local.get $character
            i32.const 32
            i32.sub
            i32.store8))
        local.get $index
        i32.const 1
        i32.add
        local.set $index
        br $next))
    local.get $pointer
    i64.extend_i32_u
    i64.const 32
    i64.shl
    local.get $length
    i64.extend_i32_u
    i64.or))
