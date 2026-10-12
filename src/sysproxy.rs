//! -system-proxy (Windows): point Windows' own proxy setting (Settings >
//! Network > Proxy, which most apps follow) at this program while it has
//! enough links, and turn it off when it has too few or when it exits, so
//! apps never get stuck pointing at a proxy that isn't there.
//!
//! The setting lives in HKCU\...\Internet Settings; after changing it,
//! WinINet is told to reload so running apps pick it up straight away.
//!
//! Turning it off on exit covers Ctrl+C and closing the console window. A
//! forced kill (Task Manager, taskkill /f) cannot be intercepted; the setting
//! then stays on until the proxy starts again or it is switched off by hand.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering::SeqCst};

use windows_sys::Win32::Networking::WinInet::{INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RegSetKeyValueW};

const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

/// Addresses that skip the proxy: this PC and private networks (router pages,
/// NAS, printers and so on).
const BYPASS: &str = "<local>;localhost;127.*;10.*;192.168.*;169.254.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;\
                      172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;172.31.*";

const UNKNOWN: u8 = 0;
const OFF: u8 = 1;
const ON: u8 = 2;

static SERVER: OnceLock<String> = OnceLock::new();
static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

/// Remembers the proxy address and makes sure the setting is switched off
/// when the program is stopped.
pub(crate) fn init(server: String) {
    let _ = SERVER.set(server);
    unsafe {
        SetConsoleCtrlHandler(Some(on_console_event), 1);
    }
}

/// Turns the setting on or off. Does nothing if it is already in that state.
pub(crate) fn set(on: bool) {
    let want = if on { ON } else { OFF };
    if STATE.swap(want, SeqCst) == want {
        return;
    }
    let result = if on {
        let server = SERVER.get().map(String::as_str).unwrap_or("127.0.0.1:8080");
        write_string("ProxyServer", server)
            .and_then(|_| write_string("ProxyOverride", BYPASS))
            .and_then(|_| write_dword("ProxyEnable", 1))
    } else {
        write_dword("ProxyEnable", 0)
    };
    match result {
        Ok(()) => {
            notify_apps();
            crate::log_info(format!("Windows proxy setting {}", if on { "ON" } else { "OFF" }));
        }
        Err(code) => {
            STATE.store(UNKNOWN, SeqCst); // try again next time
            crate::log_warn(format!("Could not change the Windows proxy setting (error {code})"));
        }
    }
}

/// Runs on its own thread when the console gets Ctrl+C, Ctrl+Break or is
/// closed. Returning 0 lets Windows go on to end the process as usual.
unsafe extern "system" fn on_console_event(_event: u32) -> windows_sys::core::BOOL {
    if STATE.load(SeqCst) == ON {
        let _ = write_dword("ProxyEnable", 0);
        notify_apps();
        STATE.store(OFF, SeqCst);
    }
    0
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn write_value(name: &str, kind: u32, data: &[u8]) -> Result<(), u32> {
    let (key, name) = (wide(KEY), wide(name));
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            kind,
            data.as_ptr().cast(),
            data.len() as u32,
        )
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

fn write_string(name: &str, value: &str) -> Result<(), u32> {
    let bytes: Vec<u8> = wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
    write_value(name, REG_SZ, &bytes)
}

fn write_dword(name: &str, value: u32) -> Result<(), u32> {
    write_value(name, REG_DWORD, &value.to_le_bytes())
}

/// Tells running apps that the proxy setting changed.
fn notify_apps() {
    unsafe {
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null(), 0);
        InternetSetOptionW(std::ptr::null(), INTERNET_OPTION_REFRESH, std::ptr::null(), 0);
    }
}
