//! unix 平台上可复用的沙箱原语（Linux / Android / macOS 共用）

// CString / OsStrExt / FsRule 仅被 landlock 使用，而 landlock 只有 Linux 目标会编译
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;

#[cfg(target_os = "linux")]
use super::FsRule;

/// 一次沙箱原语的结果，统一区分「成功 / 权限不足 / 其它失败」。
///
/// unix 侧所有 L1 原语（setrlimit / prctl / unshare / landlock / sandbox_init / ptrace）
/// 在非特权环境下失败时多为 `EACCES`/`EPERM`——归为 `Denied`；其余归为 `Failed`（不支持）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyscallOutcome {
    Applied,
    Denied,
    Failed,
}

impl SyscallOutcome {
    /// 成功/失败判定，并在失败时**立即**读取 errno 分类（须紧邻 syscall，中间不得再调用 libc）。
    pub fn from_ret(ret: i64) -> Self {
        if ret == 0 {
            return SyscallOutcome::Applied;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EACCES) | Some(libc::EPERM) => SyscallOutcome::Denied,
            _ => SyscallOutcome::Failed,
        }
    }

    pub fn is_applied(self) -> bool {
        matches!(self, SyscallOutcome::Applied)
    }
}

/// 把原语结果统一映射为 [`super::ApplyStatus`]（未请求时恒 `Skipped`）。
pub fn status(requested: bool, outcome: SyscallOutcome) -> super::ApplyStatus {
    if !requested {
        return super::ApplyStatus::Skipped;
    }
    match outcome {
        SyscallOutcome::Applied => super::ApplyStatus::Applied,
        SyscallOutcome::Denied => super::ApplyStatus::Denied,
        SyscallOutcome::Failed => super::ApplyStatus::Error,
    }
}

/// 打开文件数上限（RLIMIT_NOFILE）。
///
/// 注意：Linux glibc 的 `setrlimit` 首参类型是 `__rlimit_resource_t`(=`c_uint`)，
/// musl / macOS 则是 `c_int`，因此不共享“固定整数类型”的辅助函数，而是在每个
/// 调用点把 `libc::RLIMIT_*` 常量与 `libc::setrlimit` 直接配对使用，
/// 类型同源即天然一致，cfg / 类型之间无冲突。
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub fn limit_open_files(max: u64) -> SyscallOutcome {
    let limit = libc::rlimit {
        rlim_cur: max as libc::rlim_t,
        rlim_max: max as libc::rlim_t,
    };
    SyscallOutcome::from_ret(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } as i64)
}

/// 最大 CPU 时间：毫秒 → 秒（向上取整）RLIMIT_CPU
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub fn limit_cpu_ms(max: u64) -> SyscallOutcome {
    let secs = max.div_ceil(1000).max(1);
    let limit = libc::rlimit {
        rlim_cur: secs as libc::rlim_t,
        rlim_max: secs as libc::rlim_t,
    };
    SyscallOutcome::from_ret(unsafe { libc::setrlimit(libc::RLIMIT_CPU, &limit) } as i64)
}

/// 单文件写入大小上限（RLIMIT_FSIZE）；超限写会被 SIGXFSZ 终止
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub fn limit_file_size(max: u64) -> SyscallOutcome {
    let limit = libc::rlimit {
        rlim_cur: max as libc::rlim_t,
        rlim_max: max as libc::rlim_t,
    };
    SyscallOutcome::from_ret(unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limit) } as i64)
}

/// 核心转储大小上限（RLIMIT_CORE）；0 表示关闭 core dump
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
pub fn limit_core_bytes(max: u64) -> SyscallOutcome {
    let limit = libc::rlimit {
        rlim_cur: max as libc::rlim_t,
        rlim_max: max as libc::rlim_t,
    };
    SyscallOutcome::from_ret(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } as i64)
}

/// 设置 no_new_privileges（Landlock / seccomp 的前置条件）
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn set_no_new_privileges() -> SyscallOutcome {
    SyscallOutcome::from_ret(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } as i64)
}

/// 防调试：禁止被 ptrace 读取（PR_SET_DUMPABLE=0）并关闭 core dump
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn anti_debug() -> SyscallOutcome {
    let dumpable =
        SyscallOutcome::from_ret(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } as i64);
    if !dumpable.is_applied() {
        return dumpable;
    }
    limit_core_bytes(0)
}

/// 完全禁止网络：unshare(CLONE_NEWNET) 进入新网络命名空间，
/// 新命名空间里只有 lo 且默认 down，从而阻断全部外部网络。
/// 需要 CAP_SYS_ADMIN；非特权用户会失败（EPERM → 权限不足 = Denied）。
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn deny_network() -> SyscallOutcome {
    SyscallOutcome::from_ret(unsafe { libc::syscall(libc::SYS_unshare, libc::CLONE_NEWNET) } as i64)
}

/// Linux Landlock 文件系统限制（内核 >= 5.13，且需要先设置 no_new_privs 或持有 CAP_SYS_ADMIN）。
///
/// 语义：
/// - `readonly_fs`：不授予写入位 → 全局只读；
/// - `deny_exec`：不授予 EXECUTE 位 → 禁止执行；
/// - `sealed`：完全不添加任何放行规则 → 封闭文件系统（全部 handled 访问被拒）；
/// - `fs_rules`：非空时进入“路径白名单”能力模式，为每个路径添加权限规则；
/// - `temp_allow_write`：额外放行应用临时目录的写入（worker 继承父进程 temp 路径）。
#[cfg(target_os = "linux")]
pub fn landlock(
    readonly_fs: bool,
    deny_exec: bool,
    sealed: bool,
    fs_rules: &[FsRule],
    temp_allow_write: bool,
) -> SyscallOutcome {
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
    // ABI 2（Linux 5.19+）/ ABI 3（Linux 6.2+）：不纳入 handled 会留下 rename/link、
    // truncate 的绕过面。
    const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
    const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;
    const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
    // PR_LANDLOCK_RESTRICT_SELF = 32（libc 尚未提供该常量）
    const PR_LANDLOCK_RESTRICT_SELF: i32 = 32;
    // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION) → ABI 版本
    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;

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
    let mut write_bits = LANDLOCK_ACCESS_FS_WRITE_FILE
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

    // 探测 ABI：REFER(≥2) / TRUNCATE(≥3) 在支持的内核上必须纳入 handled 并随写权限授予。
    // Probe the ABI; on supporting kernels REFER/TRUNCATE must be handled and granted with
    // write access, otherwise rename/link/truncate stay unmediated.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<LandlockRulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION as usize,
        )
    };
    if abi >= 2 {
        write_bits |= LANDLOCK_ACCESS_FS_REFER;
    }
    if abi >= 3 {
        write_bits |= LANDLOCK_ACCESS_FS_TRUNCATE;
    }

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
        return SyscallOutcome::from_ret(ruleset as i64);
    }

    // 添加一条 path_beneath 规则；失败即整体放弃
    let add_rule = |parent_fd: i32, allowed: u64| -> SyscallOutcome {
        let beneath = LandlockPathBeneathAttr {
            allowed_access: allowed,
            parent_fd,
        };
        SyscallOutcome::from_ret(unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset,
                LANDLOCK_RULE_PATH_BENEATH,
                &beneath as *const LandlockPathBeneathAttr,
                0,
            )
        } as i64)
    };

    let mut partial = false;
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
            let root = unsafe { libc::open(c"/".as_ptr(), libc::O_PATH) };
            if root < 0 {
                let outcome = SyscallOutcome::from_ret(root as i64);
                unsafe { libc::close(ruleset as i32) };
                return outcome;
            }
            let outcome = add_rule(root, allowed);
            unsafe { libc::close(root) };
            if !outcome.is_applied() {
                unsafe { libc::close(ruleset as i32) };
                return outcome;
            }
        }
    } else {
        // 路径授权模式（sealed 基线上的显式 grant）：为每个授权路径添加能力规则，
        // 未授权的路径在基线（sealed/只读/禁执行）下依然被拒
        for rule in fs_rules {
            // 读权限必须由 rule.read 显式授予：write-only 规则不得顺带读到内容
            let mut allowed = 0u64;
            if rule.read {
                allowed |= read_bits;
                if !deny_exec {
                    allowed |= LANDLOCK_ACCESS_FS_EXECUTE;
                }
            }
            if rule.write && !readonly_fs {
                allowed |= write_bits;
            }
            if allowed == 0 {
                // 无有效授权（空规则 / write-only 遇全局只读）
                continue;
            }
            let Ok(c_path) = CString::new(rule.path.as_os_str().as_bytes()) else {
                partial = true;
                continue;
            };
            let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH) };
            if fd < 0 {
                // 路径不存在或不可达：该条授权未生效，记为部分失败而非静默 Applied。
                // Path missing/unreachable: mark partial instead of silently reporting Applied.
                partial = true;
                continue;
            }
            let outcome = add_rule(fd, allowed);
            unsafe { libc::close(fd) };
            if !outcome.is_applied() {
                unsafe { libc::close(ruleset as i32) };
                return outcome;
            }
        }
    }

    // 临时目录写放行（sealed 基线上的显式 grant）
    if temp_allow_write && !readonly_fs {
        match CString::new(std::env::temp_dir().as_os_str().as_bytes()) {
            Ok(temp) => {
                let fd = unsafe { libc::open(temp.as_ptr(), libc::O_PATH) };
                if fd >= 0 {
                    let outcome = add_rule(fd, read_bits | write_bits);
                    unsafe { libc::close(fd) };
                    if !outcome.is_applied() {
                        unsafe { libc::close(ruleset as i32) };
                        return outcome;
                    }
                } else {
                    partial = true;
                }
            }
            Err(_) => partial = true,
        }
    }

    // 应用规则集到自身进程
    let rc = unsafe { libc::prctl(PR_LANDLOCK_RESTRICT_SELF, ruleset, 0, 0, 0) };
    let outcome = SyscallOutcome::from_ret(rc as i64);
    unsafe { libc::close(ruleset as i32) };
    if partial && outcome.is_applied() {
        // 有授权未生效：不要报 Applied（fail-closed 语义）。
        // Some grants were not applied: do not report Applied.
        SyscallOutcome::Failed
    } else {
        outcome
    }
}

/// seccomp 严格模式：仅对 x86_64 生效，安装 BPF 过滤器，
/// 拒绝一组危险内核 / 进程自省类 syscall，其余全部放行。
/// 前置条件也是 no_new_privs（或 CAP_SYS_ADMIN），这里自动补上。
///
/// 其它架构不安装过滤器（返回 Failed → 该配置项 Error，保持“广兼容”）。
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn strict_syscalls() -> SyscallOutcome {
    #[cfg(target_arch = "x86_64")]
    {
        const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
        const SECCOMP_MODE_FILTER: i32 = 2;
        const SECCOMP_RET_ALLOW: u32 = 0x7FFF_0000;
        const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

        // 拒之系统调用（x86_64 号）——进程/内核自省与特权操纵类
        const DENIED: [u32; 38] = [
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
            272, // unshare
            308, // setns
            425, // io_uring_setup
            426, // io_uring_enter
            427, // io_uring_register
            428, // open_tree
            429, // move_mount
            430, // fsopen
            431, // fsconfig
            432, // fsmount
            433, // fspick
            438, // pidfd_getfd
            440, // process_madvise
            442, // mount_setattr
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
        SyscallOutcome::from_ret(unsafe {
            libc::prctl(
                libc::PR_SET_SECCOMP,
                SECCOMP_MODE_FILTER,
                &fprog as *const SockFprog,
                0,
                0,
            )
        } as i64)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        SyscallOutcome::Failed
    }
}
