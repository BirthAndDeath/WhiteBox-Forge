//! 集成测试共享的 p2 组件 guest。
//!
//! 每个测试二进制 `mod common;` 只用到其中一部分常量，故整体允许未使用。
//! 命令式世界（导出 `wasi:cli/run`），导入 `wasi:random/random` 并在 run 内调用
//! `get-random-u64`：导入强制实例化时经 p2 linker 解析该接口（验证 p2 host 接口名/版本接线），
//! 实际调用失败会 trap 使测试失败。线程回退（`try.rs`）与进程 worker（`worker.rs`）共用同一 guest。
//! `SPIN_COMMAND_COMPONENT` 供有界等待/关闭测试（`timeout.rs`）使用。
#![allow(dead_code)]
pub const RANDOM_COMMAND_COMPONENT: &str = r#"
    (component
        (import "wasi:random/random@0.2.0" (instance $rand
            (export "get-random-bytes" (func (param "len" u64) (result (list u8))))
            (export "get-random-u64" (func (result u64)))))

        (alias export $rand "get-random-u64" (func $get_random_u64))
        (core func $get_random_u64_core (canon lower (func $get_random_u64)))

        (core module $m
            (import "rand" "get-random-u64" (func $r (result i64)))
            (func (export "run") (result i32)
                call $r
                drop
                i32.const 0))

        (core instance $i
            (instantiate $m
                (with "rand" (instance (export "get-random-u64" (func $get_random_u64_core))))))

        (type $run_ty (func (result (result))))
        (func $run (type $run_ty) (canon lift (core func $i "run")))

        (instance $run_iface (export "run" (func $run)))
        (export "wasi:cli/run@0.2.0" (instance $run_iface))
    )
"#;

/// 永不返回的命令式组件（`run` 死循环），用于验证有界等待/关闭能打断纯计算。
pub const SPIN_COMMAND_COMPONENT: &str = r#"
    (component
        (core module $m
            (func (export "run") (result i32)
                (loop $l (br $l))
                unreachable))

        (core instance $i (instantiate $m))

        (type $run_ty (func (result (result))))
        (func $run (type $run_ty) (canon lift (core func $i "run")))

        (instance $run_iface (export "run" (func $run)))
        (export "wasi:cli/run@0.2.0" (instance $run_iface))
    )
"#;
