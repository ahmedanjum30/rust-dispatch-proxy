//! Starting and stopping the proxy, the start-at-login entry, and opening
//! the status page or log.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};

use crate::settings::{self, Settings};

/// Run console programs without opening a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PROXY_EXE: &str = "rust-dispatch-proxy.exe";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "rust-dispatch-proxy";

/// rust-dispatch-proxy.exe, expected next to this program.
pub fn proxy_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(PROXY_EXE)))
        .ok_or("Could not locate this program's folder")?;
    if exe.exists() {
        Ok(exe)
    } else {
        Err(format!("{PROXY_EXE} not found next to proxy-gui.exe ({})", exe.display()))
    }
}

fn hidden(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// Starts the proxy in the background with `settings`.
pub fn start(settings: &Settings) -> Result<Child, String> {
    let exe = proxy_exe()?;
    std::fs::create_dir_all(settings::data_dir()).map_err(|e| format!("Could not create the settings folder: {e}"))?;
    hidden(exe)
        .args(settings.args())
        .spawn()
        .map_err(|e| format!("Could not start the proxy: {e}"))
}

/// Stops the proxy listening on `port` (and no other program or proxy), then
/// switches Windows' proxy setting off if it still points at it: a process
/// that is ended from outside can't do that itself.
pub fn stop(port: u16) -> Result<(), String> {
    let pid = listener_pid(port).ok_or(format!("No program is listening on port {port}"))?;
    let name = process_path(pid).unwrap_or_default();
    if !name.to_ascii_lowercase().ends_with(PROXY_EXE) {
        return Err(format!("Port {port} belongs to another program ({name}), not the proxy"));
    }
    terminate(pid)?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    let exe = proxy_exe()?;
    hidden(exe)
        .args(["-system-proxy-off", "-lport", &port.to_string()])
        .status()
        .map_err(|e| format!("Could not reset the Windows proxy setting: {e}"))?;
    Ok(())
}

/// Addresses of usable network adapters, from `rust-dispatch-proxy -list`.
pub fn detect_addresses() -> Result<String, String> {
    let out = hidden(proxy_exe()?)
        .arg("-list")
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| format!("Could not list adapters: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let ips: Vec<&str> = text.lines().filter_map(|l| l.split("IPv4:").nth(1)).map(str::trim).collect();
    if ips.is_empty() {
        Err("No network adapters with an IPv4 address found".into())
    } else {
        Ok(ips.join(" "))
    }
}

/// The process listening on TCP `port` (IPv4), if any.
fn listener_pid(port: u16) -> Option<u32> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER};
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    let mut size = 0u32;
    unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_LISTENER, 0) };
    // u32 elements keep the buffer aligned for the table's u32 fields.
    let mut buf = vec![0u32; (size as usize).div_ceil(4) + 64];
    size = (buf.len() * 4) as u32;
    let status = unsafe { GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_LISTENER, 0) };
    if status != 0 {
        return None;
    }
    let table = buf.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
    let rows = unsafe { std::slice::from_raw_parts((*table).table.as_ptr(), (*table).dwNumEntries as usize) };
    // dwLocalPort holds the port in network byte order in its low 16 bits.
    rows.iter().find(|r| u16::from_be(r.dwLocalPort as u16) == port).map(|r| r.dwOwningPid)
}

fn process_path(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn terminate(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return Err("Not allowed to stop the proxy (was it started as administrator?)".into());
        }
        let ok = TerminateProcess(handle, 1);
        CloseHandle(handle);
        if ok != 0 { Ok(()) } else { Err("Could not stop the proxy".into()) }
    }
}

/// Opens a URL or file with its default program.
pub fn open(target: &str) {
    let _ = Command::new("explorer.exe").arg(target).spawn();
}

// ------------------------------------------------------------------ start at login

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The command the Run key should hold: this program in background mode.
fn login_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" --background", exe.display()))
}

/// Whether Windows starts the proxy (through this program) at login.
pub fn starts_at_login() -> bool {
    let (key, value) = (wide(RUN_KEY), wide(RUN_VALUE));
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return false;
    }
    let len = (size as usize / 2).saturating_sub(1);
    let current = String::from_utf16_lossy(&buf[..len.min(buf.len())]);
    login_command().is_some_and(|cmd| current.eq_ignore_ascii_case(&cmd))
}

pub fn set_start_at_login(on: bool) -> Result<(), String> {
    let (key, value) = (wide(RUN_KEY), wide(RUN_VALUE));
    let status = if on {
        let cmd = login_command().ok_or("Could not locate this program")?;
        let data: Vec<u8> = wide(&cmd).iter().flat_map(|c| c.to_le_bytes()).collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                data.len() as u32,
            )
        }
    } else {
        match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) } {
            2 => 0, // ERROR_FILE_NOT_FOUND: already off
            s => s,
        }
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!("Could not change the start-at-login setting (error {status})"))
    }
}
