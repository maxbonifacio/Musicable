//! How much memory Musicable uses, for agents watching a long run: see
//! `get_performance` and `musicable soak`.

use musicable_mcp::Memory;

/// Bytes in a megabyte, as the tools give memory.
#[cfg(any(windows, target_os = "linux"))]
const MB: f64 = 1_000_000.0;

/// How much memory this process uses, where the system says.
#[cfg(windows)]
pub(crate) fn used() -> Option<Memory> {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the counters are as big as `cb` says, and the process's
    // pseudo handle needs no closing.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    }
    .ok()?;
    Some(Memory {
        resident_mb: counters.WorkingSetSize as f64 / MB,
        private_mb: Some(counters.PrivateUsage as f64 / MB),
    })
}

/// How much memory this process uses: what's resident, from
/// `/proc/self/statm`.
#[cfg(target_os = "linux")]
pub(crate) fn used() -> Option<Memory> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let mut fields = statm.split_whitespace();
    let _size = fields.next()?;
    let resident: f64 = fields.next()?.parse().ok()?;
    // SAFETY: sysconf only reads a setting.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = if page > 0 { page as f64 } else { 4096.0 };
    Some(Memory {
        resident_mb: resident * page / MB,
        private_mb: None,
    })
}

/// Elsewhere, the system isn't asked yet.
#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn used() -> Option<Memory> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn a_running_process_uses_some_memory() {
        let used = super::used().expect("the system says");
        assert!(used.resident_mb > 1.0, "{used:?}");
        if cfg!(windows) {
            assert!(used.private_mb.is_some_and(|mb| mb > 1.0), "{used:?}");
        }
    }
}
