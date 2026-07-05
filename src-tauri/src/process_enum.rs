//! Running-process enumeration, used to detect Discord audio sources.
//!
//! Discord can run as `Discord.exe`, `DiscordCanary.exe`, `DiscordPTB.exe`, or in
//! a browser. We surface every match so the UI lets the player choose rather than
//! hard-coding one executable name.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
}

/// Process names we treat as Discord candidates (lowercased prefix match).
const DISCORD_HINTS: &[&str] = &["discord"];

pub fn is_discord(name: &str) -> bool {
    let lower = name.to_lowercase();
    DISCORD_HINTS.iter().any(|h| lower.starts_with(h))
}

#[cfg(windows)]
mod imp {
    use super::ProcessInfo;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
    use windows::Win32::System::ProcessStatus::EnumProcesses;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub fn list_processes() -> Vec<ProcessInfo> {
        let mut pids = vec![0u32; 4096];
        let mut needed: u32 = 0;
        unsafe {
            if EnumProcesses(
                pids.as_mut_ptr(),
                (pids.len() * std::mem::size_of::<u32>()) as u32,
                &mut needed,
            )
            .is_err()
            {
                return Vec::new();
            }
        }
        let count = needed as usize / std::mem::size_of::<u32>();
        let mut out = Vec::with_capacity(count);
        for &pid in pids.iter().take(count) {
            if pid == 0 {
                continue;
            }
            if let Some(name) = name_for_pid(pid) {
                out.push(ProcessInfo { pid, name });
            }
        }
        out
    }

    fn name_for_pid(pid: u32) -> Option<String> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = vec![0u16; MAX_PATH as usize];
            let mut size = buf.len() as u32;
            let result = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            );
            let _ = CloseHandle(handle);
            result.ok()?;
            let full = String::from_utf16_lossy(&buf[..size as usize]);
            Some(full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string())
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::ProcessInfo;
    pub fn list_processes() -> Vec<ProcessInfo> {
        Vec::new()
    }
}

pub fn list_processes() -> Vec<ProcessInfo> {
    imp::list_processes()
}

/// All running Discord-like processes.
pub fn discord_processes() -> Vec<ProcessInfo> {
    list_processes()
        .into_iter()
        .filter(|p| is_discord(&p.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_discord_variants() {
        assert!(is_discord("Discord.exe"));
        assert!(is_discord("DiscordCanary.exe"));
        assert!(is_discord("discordptb.exe"));
        assert!(!is_discord("cs2.exe"));
        assert!(!is_discord("chrome.exe"));
    }
}
