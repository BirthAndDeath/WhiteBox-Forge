//! unix 平台上可复用的沙箱原语（Linux / Android / macOS 共用）

// CString / OsStrExt / FsRule 仅被 landlock 使用，而 landlock 只有 Linux 目标会编译
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;

#[cfg(target_os = "linux")]
use super::FsRule;

/// 打开文件数上限（RLIMIT_NOFILE）。
///
/// 注意：Linux glibc 的 `setrlimit` 首参类型是 `__rlimit_resource_t`(=`c_uint`)，
/// musl / macOS 则是 `c_int`，因此不共享“固定整数类型”的辅助函数，而是在每个
/// 调用点把 `libc::RLIMIT_*` 常量与 `libc::setrlimit` 直接配对使用，
/// 类型同源即天然一致，cfg / 类型之间无冲突。
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub fn limit_open_files(max: u64) -> bool {
    let limit = libc::rlimit {
        rlim_cur: max as libc::rlim_t,
        rlim_max: max as libc::rlim_t,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) == 0 }
}

/// 最大 CPU 时间：毫秒 → 秒（向上取整）RLIMIT_CPU
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub fn limit_cpu_ms(max: u64) -> bool {
    let secs = max.div_ceil(1000).max(1);
    let limit = libc::rlimit {
        rlim_cur: secs as libc::rlim_t,
        rlim_max: secs as libc::rlim_t,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_CPU, &limit) == 0 }
}

/// 单文件写入大小上限（RLIMIT_FSIZE）；超限写会被 SIGXFSZ 终止
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub fn limit_file_size(max: u64) -> bool {
    let limit = libc::rlimit {
        rlim_cur: max as libc::rlim_t,
        rlim_max: max as libc::rlim_t,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limit) == 0 }
}

/// 核心转储大小上限（RLIMIT_CORE）；0 表示关闭 core dump
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub fn limit_core_bytes(max: u64) -> bool {
    let limit = libc::rlimit {
        rlim_cur: max as libc::rlim_t,
        rlim_max: max as libc::rlim_t,
    };
    unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) == 0 }
}

/// 设置 no_new_privileges（Landlock / seccomp 的前置条件）
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn set_no_new_privileges() -> bool {
    unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0 }
}

/// 防调试：禁止被 ptrace 读取（PR_SET_DUMPABLE=0）并关闭 core dump
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn anti_debug() -> bool {
    let dumpable = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0;
    dumpable && limit_core_bytes(0)
}

/// 完全禁止网络：unshare(CLONE_NEWNET) 进入新网络命名空间，
/// 新命名空间里只有 lo 且默认 down，从而阻断全部外部网络。
/// 需要 CAP_SYS_ADMIN；非特权用户会失败（返回 false → 对应项 Error）。
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn deny_network() -> bool {
    unsafe { libc::syscall(libc::SYS_unshare, libc::CLONE_NEWNET) == 0 }
}

/// Linux Landlock 文件系统限制（内核 >= 5.13，且需要先设置 no_new_privs 或持有 CAP_SYS_ADMIN）。
///
/// 语义：
/// - `readonly_fs`：不授予写入位 → 全局只读；
/// - `deny_exec`：不授予 EXECUTE 位 → 禁止执行；
/// - `sealed`：完全不添加任何放行规则 → 封闭文件系统（全部 handled 访问被拒）；
/// - `fs_rules`：非空时进入“路径白名单”能力模式，为每个路径添加权限规则；
/// - `temp_allow_write`：额外放行系统临时目录的写入（用于只读 / 白名单组合）。
#[cfg(target_os = "linux")]
pub fn landlock(
    readonly_fs: bool,
    deny_exec: bool,
    sealed: bool,
    fs_rules: &[FsRule],
    temp_allow_write: bool,
) -> bool {
    const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
    const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
    const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
    const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
    const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
    const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
    const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
    const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
    const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
    const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
    const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
    const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
    const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
    const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
    // PR_LANDLOCK_RESTRICT_SELF = 32（libc 尚未提供该常量）
    const PR_LANDLOCK_RESTRICT_SELF: i32 = 32;

    #[repr(C)]
    struct LandlockRulesetAttr {
        handled_access_fs: u64,
    }
    #[repr(C)]
    struct LandlockPathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    let read_bits = LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;
    let write_bits = LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_REMOVE_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_FILE
        | LANDLOCK_ACCESS_FS_MAKE_REG
        | LANDLOCK_ACCESS_FS_MAKE_DIR
        | LANDLOCK_ACCESS_FS_MAKE_SYM
        | LANDLOCK_ACCESS_FS_MAKE_CHAR
        | LANDLOCK_ACCESS_FS_MAKE_BLOCK
        | LANDLOCK_ACCESS_FS_MAKE_FIFO
        | LANDLOCK_ACCESS_FS_MAKE_SOCK;

    // Landlock 需要本进程已设置 no_new_privs（或持有 CAP_SYS_ADMIN），这里补上（幂等）。
    let _ = set_no_new_privileges();

    let handled: u64 = LANDLOCK_ACCESS_FS_EXECUTE | read_bits | write_bits;
    let attr = LandlockRulesetAttr {
        handled_access_fs: handled,
    };
    let ruleset = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const LandlockRulesetAttr,
            std::mem::size_of::<LandlockRulesetAttr>(),
            0,
        )
    };
    if ruleset < 0 {
        return false;
    }

    // 添加一条 path_beneath 规则；失败即整体放弃
    let mut add_rule = |parent_fd: i32, allowed: u64| -> bool {
        let beneath = LandlockPathBeneathAttr {
            allowed_access: allowed,
            parent_fd,
        };
        unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset,
                LANDLOCK_RULE_PATH_BENEATH,
                &beneath as *const LandlockPathBeneathAttr,
                0,
            ) == 0
        }
    };

    if fs_rules.is_empty() {
        // 无路径白名单：sealed 时不添加任何规则 → 全部拒绝；非 sealed 时对 "/" 放行 read（+ 按需 write/exec）
        if !sealed {
            let mut allowed = read_bits;
            if !readonly_fs {
                allowed |= write_bits;
            }
            if !deny_exec {
                allowed |= LANDLOCK_ACCESS_FS_EXECUTE;
            }
            let root = libc::open(c"/".as_ptr().cast(), libc::O_PATH);
            let ok = root >= 0 && add_rule(root, allowed);
            if root >= 0 {
                unsafe { libc::close(root) };
            }
            if !ok {
                unsafe { libc::close(ruleset as i32) };
                return false;
            }
        }
    } else {
        // 路径授权模式（sealed 基线上的显式 grant）：为每个授权路径添加能力规则，
        // 未授权的路径在基线（sealed/只读/禁执行）下依然被拒
        for rule in fs_rules {
            if !rule.read && !rule.write {
                continue;
            }
            let mut allowed = read_bits;
            if rule.read && !deny_exec {
                allowed |= LANDLOCK_ACCESS_FS_EXECUTE;
            }
            if rule.write && !readonly_fs {
                allowed |= write_bits;
            }
            let Ok(c_path) = CString::new(rule.path.as_os_str().as_bytes()) else {
                continue;
            };
            let fd = libc::open(c_path.as_ptr().cast(), libc::O_PATH);
            if fd < 0 {
                continue; // 路径不存在或不可达：跳过（该条能力授予失败而非整体失败）
            }
            let ok = add_rule(fd, allowed);
            unsafe { libc::close(fd) };
            if !ok {
                unsafe { libc::close(ruleset as i32) };
                return false;
            }
        }
    }

    // 临时目录写放行（sealed 基线上的显式 grant）
    if temp_allow_write && !readonly_fs {
        if let Ok(temp) = CString::new(std::env::temp_dir().as_os_str().as_bytes()) {
            let fd = libc::open(temp.as_ptr().cast(), libc::O_PATH);
            if fd >= 0 {
                let ok = add_rule(fd, read_bits | write_bits);
                unsafe { libc::close(fd) };
                if !ok {
                    unsafe { libc::close(ruleset as i32) };
                    return false;
                }
            }
        }
    }

    // 应用规则集到自身进程
    let rc = unsafe { libc::prctl(PR_LANDLOCK_RESTRICT_SELF, ruleset, 0, 0, 0) };
    unsafe { libc::close(ruleset as i32) };
    rc == 0
}

/// seccomp 严格模式：仅对 x86_64 生效，安装 BPF 过滤器，
/// 拒绝一组危险内核 / 进程自省类 syscall，其余全部放行。
/// 前置条件也是 no_new_privs（或 CAP_SYS_ADMIN），这里自动补上。
///
/// 其它架构不安装过滤器（返回 false → 该配置项 Error，保持“广兼容”）。
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn strict_syscalls() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
        const SECCOMP_MODE_FILTER: i32 = 2;
        const SECCOMP_RET_ALLOW: u32 = 0x7FFF_0000;
        const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

        // 拒之系统调用（x86_64 号）——进程/内核自省与特权操纵类
        const DENIED: [u32; 24] = [
            101, // ptrace
            161, // chroot
            298, // perf_event_open
            304, // open_by_handle_at
            310, // process_vm_readv
            311, // process_vm_writev
            321, // bpf
            323, // userfaultfd
            175, // init_module
            176, // delete_module
            313, // finit_module
            248, // add_key
            249, // request_key
            250, // keyctl
            168, // swapoff
            167, // swapon
            165, // mount
            166, // umount2
            154, // modify_ldt
            173, // ioperm
            172, // iopl
            246, // kexec_load
            320, // kexec_file_load
            155, // pivot_root
        ];

        #[derive(Clone, Copy)]
        #[repr(C)]
        struct SockFilter {
            code: u16,
            jt: u8,
            jf: u8,
            k: u32,
        }
        #[repr(C)]
        struct SockFprog {
            len: u16,
            filter: *const SockFilter,
        }

        // BPF 指令码：经典 8 位常量直接 OR 组合，高位字节为 0（struct sock_filter.code）
        const BPF_LD_ABS_W: u16 = 0x20; // BPF_LD(0x00) | BPF_W(0x00) | BPF_ABS(0x20)
        const BPF_JMP_JEQ_K: u16 = 0x15; // BPF_JMP(0x05) | BPF_JEQ(0x10) | BPF_K(0x00)
        const BPF_RET_K: u16 = 0x06; // BPF_RET(0x06) | BPF_K(0x00)

        fn ld_abs(offset: u8) -> SockFilter {
            SockFilter {
                code: BPF_LD_ABS_W,
                jt: 0,
                jf: 0,
                k: offset as u32,
            }
        }
        fn jeq(k: u32, jt: u8, jf: u8) -> SockFilter {
            SockFilter {
                code: BPF_JMP_JEQ_K,
                jt,
                jf,
                k,
            }
        }
        fn ret(k: u32) -> SockFilter {
            SockFilter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k,
            }
        }

        let mut prog = Vec::with_capacity(DENIED.len() * 2 + 3);
        prog.push(ld_abs(4)); // seccomp_data.arch
        prog.push(jeq(AUDIT_ARCH_X86_64, 1, 0)); // 匹配则跳到 #3（load nr），否则落到 ALLOW
        prog.push(ret(SECCOMP_RET_ALLOW)); // 非 x86_64：放行（保持广兼容）
        prog.push(ld_abs(0)); // seccomp_data.nr
        for nr in DENIED {
            prog.push(jeq(nr, 0, 1)); // 命中 → 下一条（ERRNO）；未命中跳过 ERRNO
            prog.push(ret(SECCOMP_RET_ERRNO | libc::EACCES as u32));
        }
        prog.push(ret(SECCOMP_RET_ALLOW));

        // seccomp 安装前置：no_new_privs
        let _ = set_no_new_privileges();

        let fprog = SockFprog {
            len: prog.len() as u16,
            filter: prog.as_ptr(),
        };
        unsafe {
            libc::prctl(
                libc::PR_SET_SECCOMP,
                SECCOMP_MODE_FILTER,
                &fprog as *const SockFprog,
                0,
                0,
            ) == 0
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}
