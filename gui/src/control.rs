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

/// This program. It contains the proxy too: `proxy-gui.exe --proxy ARGS`
/// runs the proxy instead of the window, so no other file is needed.
fn this_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("Could not locate this program: {e}"))
}

/// This program in proxy mode, without a window.
fn proxy_command() -> Result<Command, String> {
    let mut cmd = Command::new(this_exe()?);
    cmd.arg("--proxy")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(cmd)
}

/// Starts the proxy in the background with `settings`.
pub fn start(settings: &Settings) -> Result<Child, String> {
    let _ = settings.write_exclude_file(); // the proxy reads it at start
    start_with(&settings.args())
}

/// Starts the proxy in the background with these proxy options.
pub fn start_with(args: &[String]) -> Result<Child, String> {
    std::fs::create_dir_all(settings::data_dir()).map_err(|e| format!("Could not create the settings folder: {e}"))?;
    proxy_command()?.args(args).spawn().map_err(|e| format!("Could not start the proxy: {e}"))
}

/// The proxy on `port` if it runs from program file `exe`.
pub fn proxy_pid_from(port: u16, exe: &std::path::Path) -> Option<u32> {
    let pid = listener_pid(port)?;
    let path = process_path(pid)?;
    path.eq_ignore_ascii_case(&exe.to_string_lossy()).then_some(pid)
}

/// The options a proxy process was started with (what follows `--proxy`).
pub fn proxy_args(pid: u32) -> Option<Vec<String>> {
    let args = command_line(pid)?;
    let at = args.iter().position(|a| a == "--proxy")?;
    Some(args[at + 1..].to_vec())
}

/// Another process's command line, split into arguments.
fn command_line(pid: u32) -> Option<Vec<String>> {
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, UNICODE_STRING};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        // The answer is a UNICODE_STRING followed by the text it points to;
        // u64 elements keep it aligned.
        let mut buf = vec![0u64; 8 * 1024];
        let mut len = 0u32;
        let status = NtQueryInformationProcess(
            handle,
            ProcessCommandLineInformation,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut len,
        );
        CloseHandle(handle);
        if status != 0 {
            return None;
        }
        let text = &*buf.as_ptr().cast::<UNICODE_STRING>();
        let mut line: Vec<u16> = std::slice::from_raw_parts(text.Buffer, text.Length as usize / 2).to_vec();
        line.push(0);
        let mut count = 0;
        let argv = CommandLineToArgvW(line.as_ptr(), &mut count);
        if argv.is_null() {
            return None;
        }
        let args = (0..count as usize)
            .map(|i| {
                let p = *argv.add(i);
                let n = (0..).take_while(|&k| *p.add(k) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
            })
            .collect();
        LocalFree(argv.cast());
        Some(args)
    }
}

/// Whether a process path is a proxy: this program (in proxy mode) or a
/// separate rust-dispatch-proxy.exe.
fn is_proxy(path: &str) -> bool {
    let this = this_exe().map(|p| p.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let path = path.to_ascii_lowercase();
    path == this || path.ends_with(&format!("\\{PROXY_EXE}"))
}

/// Stops the proxy listening on `port` (and no other program or proxy), then
/// switches Windows' proxy setting off if it still points at it: a process
/// that is ended from outside can't do that itself.
pub fn stop(port: u16) -> Result<(), String> {
    let pid = listener_pid(port).ok_or(format!("No program is listening on port {port}"))?;
    let path = process_path(pid).unwrap_or_default();
    if !is_proxy(&path) {
        return Err(format!("Port {port} belongs to another program ({path}), not the proxy"));
    }
    stop_pid(pid, port)
}

/// Ends proxy process `pid` and switches Windows' proxy setting off if it
/// still points at port `port`.
pub fn stop_pid(pid: u32, port: u16) -> Result<(), String> {
    terminate(pid)?;
    std::thread::sleep(std::time::Duration::from_millis(300));
    proxy_command()?
        .args(["-system-proxy-off", "-lport", &port.to_string()])
        .status()
        .map_err(|e| format!("Could not reset the Windows proxy setting: {e}"))?;
    Ok(())
}

/// Addresses of usable network adapters, from the proxy's `-list`.
pub fn detect_addresses() -> Result<String, String> {
    let out = proxy_command()?
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

// ------------------------------------------------------------------ Windows apps

// Microsoft Store and other Windows (Store) apps run sandboxed, and Windows
// blocks them from connecting to programs on the same PC, including this
// proxy at 127.0.0.1. With the system proxy on they then can't connect at
// all, unless each one is given a "loopback exemption".

const STORE_FAMILY: &str = "microsoft.windowsstore_8wekyb3d8bbwe";

/// Whether Microsoft Store may reach the proxy (taken as the sign that
/// Windows apps were allowed). None if Windows couldn't be asked.
pub fn windows_apps_allowed() -> Option<bool> {
    let out = Command::new("CheckNetIsolation.exe")
        .args(["LoopbackExempt", "-s"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).to_ascii_lowercase().contains(STORE_FAMILY))
}

/// Lets every installed Windows app reach the proxy. Asks for administrator
/// permission once (a UAC prompt) and waits until it's done. Apps installed
/// later need this again.
pub fn allow_windows_apps() -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NO_CONSOLE, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    const SCRIPT: &str = "Get-AppxPackage | Where-Object { -not $_.IsFramework } | ForEach-Object { \
                          CheckNetIsolation.exe LoopbackExempt -a ('-n=' + $_.PackageFamilyName) | Out-Null }";
    let params = wide(&format!("-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -Command \"{SCRIPT}\""));
    let (verb, file) = (wide("runas"), wide("powershell.exe"));

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    if unsafe { ShellExecuteExW(&mut info) } == 0 || info.hProcess.is_null() {
        return Err("Administrator permission wasn't given, so Windows apps still can't use the proxy.".into());
    }
    let mut code = 1u32;
    unsafe {
        WaitForSingleObject(info.hProcess, 5 * 60 * 1000);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    if code == 0 {
        Ok(())
    } else {
        Err(format!("Allowing Windows apps failed (code {code})"))
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
    login_entry().is_some_and(|current| login_command().is_some_and(|cmd| current.eq_ignore_ascii_case(&cmd)))
}

/// The program the start-at-login entry runs, if there is one.
fn login_program() -> Option<PathBuf> {
    let entry = login_entry()?;
    let path = entry.strip_prefix('"')?.split('"').next()?;
    Some(PathBuf::from(path))
}

/// Points the start-at-login entry at this program if it ran `old` (after
/// an update) or a program that no longer exists (e.g. an older version
/// deleted by hand). An entry for another, existing file is left alone.
pub fn repoint_login(old: Option<&std::path::Path>) {
    let Some(program) = login_program() else { return };
    let was_old = old.is_some_and(|old| program.to_string_lossy().eq_ignore_ascii_case(&old.to_string_lossy()));
    if (was_old || !program.exists()) && !starts_at_login() {
        let _ = set_start_at_login(true);
    }
}

/// The start-at-login entry's command, if there is one.
fn login_entry() -> Option<String> {
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
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len.min(buf.len())]))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_proxies() {
        assert!(is_proxy(r"C:\multi\rust-dispatch-proxy.exe"));
        assert!(is_proxy(r"C:\MULTI\RUST-DISPATCH-PROXY.EXE"));
        assert!(is_proxy(&this_exe().unwrap().to_string_lossy()));
        assert!(!is_proxy(r"C:\Program Files\Other\server.exe"));
        assert!(!is_proxy(r"C:\x\not-rust-dispatch-proxy.exe.bak"));
    }

    #[test]
    fn reads_a_command_line() {
        let args = command_line(std::process::id()).unwrap();
        assert_eq!(args.len(), std::env::args().count());
        let mut child = Command::new("cmd.exe")
            .args(["/c", "ping -n 3 127.0.0.1 >nul", "--proxy", "-lport", "8080", "a b"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let after = proxy_args(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(after.unwrap(), ["-lport", "8080", "a b"]);
    }

    #[test]
    fn can_ask_windows_about_windows_apps() {
        // Answers either way, without administrator rights.
        assert!(windows_apps_allowed().is_some());
    }
}
