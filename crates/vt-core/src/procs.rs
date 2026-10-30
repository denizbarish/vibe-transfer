//! Whether an AI coding tool is running on this machine.

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use crate::manifest::Tool;

/// Process name of each tool's CLI.
fn process_name(tool: Tool) -> &'static str {
    match tool {
        Tool::Claude => "claude",
        Tool::Codex => "codex",
    }
}

/// PIDs of the running processes of `tool`, sorted. Empty when none runs
/// or the process list cannot be read.
pub fn running(tool: Tool) -> Vec<u32> {
    let Ok(mut sys) = System::new() else {
        return Vec::new();
    };
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let mut pids: Vec<u32> = sys
        .processes_by_exact_name(process_name(tool).as_ref())
        .map(|p| p.pid().as_u32())
        .collect();
    pids.sort_unstable();
    pids
}

/// This machine's host name, when it can be read.
pub fn host_name() -> Option<String> {
    System::host_name().ok().filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A copy of `sleep` named `claude` stands in for a running Claude.
    #[cfg(unix)]
    #[test]
    fn finds_process_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("claude");
        std::fs::copy("/bin/sleep", &fake).unwrap();
        let mut child = std::process::Command::new(&fake).arg("30").spawn().unwrap();
        let found = running(Tool::Claude);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(found.contains(&child.id()), "{found:?}");
    }
}
