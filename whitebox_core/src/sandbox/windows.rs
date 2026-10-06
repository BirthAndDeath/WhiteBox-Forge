//! Windows 平台进程沙箱实现。
//!
//! 应用机制（全部无需管理员权限）：
//! - no_new_privileges：Job 对象安全限制（`JOB_OBJECT_SECURITY_NO_ADMIN`）
//! - max_children / deny_exec：Job 活动进程数上限（deny_exec = 1）
//! - max_cpu_time：Job 每进程 CPU 时间上限（`JOB_OBJECT_LIMIT_PROCESS_TIME`）
//! - max_memory：Job 提交内存上限（`JOB_OBJECT_LIMIT_JOB_MEMORY`）
//! - no_win32k / ms_signed_only / strict_handles：`SetProcessMitigationPolicy`
//!
//! 文件系统能力组、网络能力组、max_open_files / max_core_size / strict_syscalls：
//! Windows 无对应非特权 API（WFP / WDAC / ACL 均需管理员或签名策略），
//! 由 [`super::ReportSet::from_config`] 自动标为“请求了 → Error”。
//!
//! Job 句柄必须存活到进程结束（`KILL_ON_JOB_CLOSE` 语义），故以 usize 存入全局 static
//! （`*mut c_void` 不是 Send/Sync，不能直接进 static）。

use std::ffi::c_void;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_TIME,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, ProcessSignaturePolicy, ProcessStrictHandleCheckPolicy,
    ProcessSystemCallDisablePolicy, SetProcessMitigationPolicy,
};

use super::SandboxCapability::{
    MaxChildren, MaxCpuTime, MaxMemory, MsSignedOnly, NoExec, NoNewPrivileges, NoWin32k,
    StrictHandles,
};
use super::{outcome, CapabilityReport, ReportSet, SandboxConfig};

// JobObjectSecurityLimitInformation = 6（Win8+ 的未文档化信息类，windows-sys 未导出）
const JOBOBJECT_INFO_SECURITY_LIMITS: i32 = 6;
// JOB_OBJECT_SECURITY_NO_ADMIN = 0x00000008
const JOB_OBJECT_SECURITY_NO_ADMIN: u32 = 0x0000_0008;
// PROCESS_MITIGATION_* 策略的位元（windows-sys 0.59 未导出命名常量，按 SDK 定义手写）
const MITIGATION_DISALLOW_WIN32K_SYSTEM_CALLS: u32 = 0x1;
const MITIGATION_MICROSOFT_SIGNED_ONLY: u32 = 0x1;
const MITIGATION_RAISE_EXCEPTIONS_ON_INVALID_HANDLE: u32 = 0x1;

/// 与 SDK 中 JOBOBJECT_SECURITY_LIMIT_INFORMATION 一致的布局
#[repr(C)]
struct JobObjectSecurityLimit {
    security_limit_flags: u32,
    job_token: HANDLE,
    permissions_to_grant: u32,
    permissions_to_deny: u32,
}

/// 全局持有的 Job 句柄（usize 形式，进程结束前不关闭）
static JOB_HANDLE: OnceLock<usize> = OnceLock::new();

/// 当前进程在某项 Job 限制上的应用结果
struct JobGuard {
    no_new_privileges: bool,
    active_process_limit: bool,
    max_memory: bool,
    max_cpu_time: bool,
}

pub(super) fn setup(config: &SandboxConfig) -> Vec<CapabilityReport> {
    let job = create_job(config);
    let mut set = ReportSet::from_config(config);
    set.override_status(
        NoExec,
        outcome(config.deny_exec, job.as_ref().is_some_and(|j| j.active_process_limit)),
    )
    .override_status(
        MaxChildren,
        outcome(
            config.max_children.is_some(),
            job.as_ref().is_some_and(|j| j.active_process_limit),
        ),
    )
    .override_status(
        NoNewPrivileges,
        outcome(
            config.no_new_privileges,
            job.as_ref().is_some_and(|j| j.no_new_privileges),
        ),
    )
    .override_status(
        MaxMemory,
        outcome(
            config.max_memory_bytes.is_some(),
            job.as_ref().is_some_and(|j| j.max_memory),
        ),
    )
    .override_status(
        MaxCpuTime,
        outcome(
            config.max_cpu_ms.is_some(),
            job.as_ref().is_some_and(|j| j.max_cpu_time),
        ),
    )
    .override_status(
        NoWin32k,
        outcome(
            config.block_win32k,
            mitigation(ProcessSystemCallDisablePolicy, MITIGATION_DISALLOW_WIN32K_SYSTEM_CALLS),
        ),
    )
    .override_status(
        MsSignedOnly,
        outcome(
            config.block_non_microsoft_binaries,
            mitigation(ProcessSignaturePolicy, MITIGATION_MICROSOFT_SIGNED_ONLY),
        ),
    )
    .override_status(
        StrictHandles,
        outcome(
            config.strict_handle_checks,
            mitigation(
                ProcessStrictHandleCheckPolicy,
                MITIGATION_RAISE_EXCEPTIONS_ON_INVALID_HANDLE,
            ),
        ),
    );
    set.finish()
}

/// 设置一条进程缓解策略。WinAPI 的 `PROCESS_MITIGATION_*_POLICY` 结构体就是
/// 4 字节的 `Flags` 联合体，用一个同布局的 `repr(C)` 单字段结构体传递即可。
fn mitigation(policy: i32, flags: u32) -> bool {
    #[repr(C)]
    struct MitiPolicy {
        flags: u32,
    }
    let p = MitiPolicy { flags };
    unsafe {
        SetProcessMitigationPolicy(
            policy,
            &p as *const MitiPolicy as *const c_void,
            std::mem::size_of::<MitiPolicy>(),
        ) != 0
    }
}

/// 若请求了任何基于 Job 的能力，创建一个 Job 对象、设置限制并绑定到当前进程。
fn create_job(config: &SandboxConfig) -> Option<JobGuard> {
    let job_requested = config.no_new_privileges
        || config.deny_exec
        || config.max_children.is_some()
        || config.max_memory_bytes.is_some()
        || config.max_cpu_ms.is_some();
    if !job_requested {
        return None;
    }

    let created = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if created.is_null() {
        return None;
    }
    let handle = *JOB_HANDLE.get_or_init(|| created as usize) as HANDLE;

    let mut guard = JobGuard {
        no_new_privileges: false,
        active_process_limit: false,
        max_memory: false,
        max_cpu_time: false,
    };

    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    let mut flags: u32 = 0;

    // 活动进程数：deny_exec → 1，否则 max_children
    let active_limit = if config.deny_exec {
        1
    } else {
        config.max_children.unwrap_or(0)
    };
    if config.deny_exec || config.max_children.is_some() {
        flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        flags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        info.BasicLimitInformation.ActiveProcessLimit = active_limit;
        guard.active_process_limit = true;
    }
    if let Some(bytes) = config.max_memory_bytes {
        flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.JobMemoryLimit = bytes as usize;
        guard.max_memory = true;
    }
    if let Some(ms) = config.max_cpu_ms {
        flags |= JOB_OBJECT_LIMIT_PROCESS_TIME;
        // 时间单位是 100ns，1ms = 10000
        info.BasicLimitInformation.PerProcessUserTimeLimit = ms as i64 * 10_000;
        guard.max_cpu_time = true;
    }
    if flags != 0 {
        info.BasicLimitInformation.LimitFlags = flags;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } != 0;
        if !ok {
            return None;
        }
    }

    if config.no_new_privileges {
        let security = JobObjectSecurityLimit {
            security_limit_flags: JOB_OBJECT_SECURITY_NO_ADMIN,
            job_token: std::ptr::null_mut(),
            permissions_to_grant: 0,
            permissions_to_deny: 0,
        };
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JOBOBJECT_INFO_SECURITY_LIMITS,
                &security as *const _ as *const c_void,
                std::mem::size_of::<JobObjectSecurityLimit>() as u32,
            )
        } != 0;
        guard.no_new_privileges = ok;
    }

    let assigned = unsafe { AssignProcessToJobObject(handle, GetCurrentProcess()) } != 0;
    if !assigned {
        return None;
    }

    Some(guard)
}