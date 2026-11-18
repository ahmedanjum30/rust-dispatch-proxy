//! Checking GitHub for a newer release, and updating proxy-gui.exe in place.
//!
//! No GitHub API: it allows only 60 requests an hour per internet address,
//! which shared or busy connections run out of. Instead, the "latest release"
//! page redirects to the newest tag, and the download address follows from
//! the tag.
//!
//! HTTPS goes through WinINet (part of Windows, and it follows the Windows
//! proxy setting), so no extra libraries are needed.

use std::path::{Path, PathBuf};

use crate::control;

use windows_sys::Win32::Networking::WinInet::{
    HTTP_QUERY_FLAG_NUMBER, HTTP_QUERY_STATUS_CODE, HttpQueryInfoW, INTERNET_FLAG_NO_CACHE_WRITE, INTERNET_FLAG_NO_UI, INTERNET_FLAG_RELOAD,
    INTERNET_OPEN_TYPE_PRECONFIG, INTERNET_OPTION_URL, InternetCloseHandle, InternetOpenUrlW, InternetOpenW, InternetQueryOptionW, InternetReadFile,
};

const REPO: &str = "https://github.com/ahmedanjum30/rust-dispatch-proxy";
pub const RELEASES_PAGE: &str = "https://github.com/ahmedanjum30/rust-dispatch-proxy/releases/latest";

/// The newest release on GitHub.
#[derive(Clone)]
pub struct Release {
    pub tag: String,
    pub page: String,
    /// This PC's proxy-gui download.
    pub download: String,
}

/// "v1.4.0" or "1.4.0" as numbers, for comparing.
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

impl Release {
    pub fn is_newer(&self) -> bool {
        match (parse_version(&self.tag), parse_version(current_version())) {
            (Some(latest), Some(ours)) => latest > ours,
            _ => false,
        }
    }

    fn from_tag(tag: &str) -> Release {
        Release {
            tag: tag.to_string(),
            page: format!("{REPO}/releases/tag/{tag}"),
            download: format!("{REPO}/releases/download/{tag}/{}", asset_name(tag)),
        }
    }
}

/// The release asset name for this build, e.g.
/// proxy-gui-v1.5.0-x86_64-pc-windows-msvc.exe.
fn asset_name(tag: &str) -> String {
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    format!("proxy-gui-{tag}-{arch}-pc-windows-msvc.exe")
}

/// The tag in a release page address: .../releases/tag/v1.4.1 -> v1.4.1.
fn tag_from_url(url: &str) -> Option<&str> {
    let tag = url.split("/releases/tag/").nth(1)?;
    let tag = tag.split(['?', '#', '/']).next()?;
    (!tag.is_empty()).then_some(tag)
}

/// Asks GitHub for the latest release.
pub fn latest() -> Result<Release, String> {
    // Only the address the page redirects to is needed, not the page itself.
    let request = Request::open(RELEASES_PAGE)?;
    request.expect_ok()?;
    let url = request.final_url().ok_or("Unexpected reply from GitHub")?;
    let tag = tag_from_url(&url).ok_or("GitHub doesn't list a release yet")?;
    Ok(Release::from_tag(tag))
}

/// Downloads the release's exe into this program's folder, under its name
/// on GitHub (e.g. proxy-gui-v1.4.3-x86_64-pc-windows-msvc.exe), and returns
/// its path. The caller starts it with `--take-over`.
pub fn install(release: &Release) -> Result<PathBuf, String> {
    let exe = this_exe()?;
    let target = exe.with_file_name(asset_name(&release.tag));
    if same_file(&target, &exe) {
        return Err("This program already has the new version's name".into());
    }
    let request = Request::open(&release.download)?;
    if request.status() == 404 {
        return Err("This release has no download for this PC".into());
    }
    request.expect_ok()?;
    let bytes = request.read_all()?;
    // An exe starts with "MZ"; anything small or different is an error page.
    if bytes.len() < 100_000 || !bytes.starts_with(b"MZ") {
        return Err("The download doesn't look like the program".into());
    }
    // Written under a temporary name first, so a half-written file never
    // has the program's name.
    let part = sibling(&target, "part");
    std::fs::write(&part, &bytes).map_err(|e| format!("Could not save the update next to this program: {e}"))?;
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&part, &target).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("Could not save the update next to this program: {e}")
    })?;
    Ok(target)
}

/// Run by the new version, started with `--take-over OLD` by the old one:
/// restarts the proxy (if OLD was running it) from this program with exactly
/// the options it had, points start-at-login here if it ran OLD, and deletes
/// OLD once its window has closed.
pub fn take_over(old: &Path, port: u16) {
    let Ok(exe) = this_exe() else { return };
    // Only ever another copy of this program, next to this one.
    if same_file(old, &exe) || old.parent() != exe.parent() || !is_program_name(old) {
        return;
    }
    move_proxy(old, port);
    control::repoint_login(Some(old));
    // The old program may still be closing, or (after a 1.4.1/1.4.2-style
    // update) about to start the proxy itself, so keep at it for a while.
    let (old, leftover) = (old.to_path_buf(), sibling(old, "old"));
    std::thread::spawn(move || {
        for _ in 0..30 {
            move_proxy(&old, port);
            let _ = std::fs::remove_file(&leftover);
            if std::fs::remove_file(&old).is_ok() || !old.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

/// If `old` runs the proxy on `port`, restarts it from this program with the
/// same options.
fn move_proxy(old: &Path, port: u16) {
    let Some(pid) = control::proxy_pid_from(port, old) else { return };
    let args = control::proxy_args(pid).map(|mut args| {
        // Versions before 1.5.1 didn't pass the switched-off connections.
        if !args.iter().any(|a| a == "-exclude-file") {
            let _ = crate::settings::Settings::load().write_exclude_file();
            args.extend(["-exclude-file".into(), crate::settings::exclude_path().to_string_lossy().into_owned()]);
        }
        args
    });
    if control::stop_pid(pid, port).is_ok() {
        let _ = match args {
            Some(args) => control::start_with(&args),
            None => control::start(&crate::settings::Settings::load()),
        };
    }
}

/// Versions 1.4.1 and 1.4.2 updated by putting the new version in place of
/// the old file, so it can carry an older version's name, e.g. a v1.4.3
/// called proxy-gui-v1.4.2-....exe. Gives this program its right name: makes
/// a copy under that name and starts it with `--take-over`, which moves the
/// proxy and start-at-login over and deletes this file. Returns true if the
/// copy was started (this one should then exit).
pub fn fix_name() -> bool {
    let Ok(exe) = this_exe() else { return false };
    let Some(version) = exe.file_name().and_then(|n| version_in_name(&n.to_string_lossy())) else {
        return false;
    };
    if parse_version(&version) == parse_version(current_version()) {
        return false;
    }
    let right = exe.with_file_name(asset_name(&format!("v{}", current_version())));
    if std::fs::copy(&exe, &right).is_err() {
        return false;
    }
    std::process::Command::new(&right).arg("--take-over").arg(&exe).spawn().is_ok()
}

/// Removes the previous version left behind by a 1.4.1/1.4.2-style update
/// (this file's name plus .old).
pub fn clean_up_old() {
    let Ok(exe) = this_exe() else { return };
    let old = sibling(&exe, "old");
    if old.exists() {
        delete_soon(vec![old]);
    }
}

/// Deletes files in the background, retrying for a few seconds: right after
/// an update the old program may still be closing.
fn delete_soon(mut files: Vec<PathBuf>) {
    if files.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..30 {
            files.retain(|f| std::fs::remove_file(f).is_err() && f.exists());
            if files.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

fn this_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("Could not locate this program: {e}"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

/// proxy-gui.exe or proxy-gui-v1.4.2-....exe: names this program has.
fn is_program_name(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    name.starts_with("proxy-gui") && name.ends_with(".exe")
}

/// The version in a release file name: proxy-gui-v1.4.2-x86_64-....exe -> "v1.4.2".
fn version_in_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("proxy-gui-")?;
    let version = rest.split('-').next()?;
    parse_version(version).map(|_| version.to_string())
}

/// proxy-gui.exe -> proxy-gui.exe.new / proxy-gui.exe.old
fn sibling(exe: &Path, ext: &str) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".");
    name.push(ext);
    PathBuf::from(name)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An HTTPS GET in progress; redirects are followed. Closed when dropped.
struct Request {
    session: *mut core::ffi::c_void,
    handle: *mut core::ffi::c_void,
}

impl Request {
    fn open(url: &str) -> Result<Request, String> {
        let agent = wide(&format!("rust-dispatch-proxy/{}", current_version()));
        let url_w = wide(url);
        unsafe {
            let session = InternetOpenW(agent.as_ptr(), INTERNET_OPEN_TYPE_PRECONFIG, std::ptr::null(), std::ptr::null(), 0);
            if session.is_null() {
                return Err("Could not start an internet connection".into());
            }
            let flags = INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE | INTERNET_FLAG_NO_UI;
            let handle = InternetOpenUrlW(session, url_w.as_ptr(), std::ptr::null(), 0, flags, 0);
            if handle.is_null() {
                InternetCloseHandle(session);
                return Err("Could not reach GitHub. Check your internet connection.".into());
            }
            Ok(Request { session, handle })
        }
    }

    fn status(&self) -> u32 {
        let mut status = 0u32;
        let mut len = 4u32;
        unsafe {
            HttpQueryInfoW(
                self.handle,
                HTTP_QUERY_STATUS_CODE | HTTP_QUERY_FLAG_NUMBER,
                (&mut status as *mut u32).cast(),
                &mut len,
                std::ptr::null_mut(),
            )
        };
        status
    }

    fn expect_ok(&self) -> Result<(), String> {
        match self.status() {
            200 => Ok(()),
            403 | 429 => Err("GitHub is limiting requests from this connection. Try again later.".into()),
            status => Err(format!("GitHub answered with status {status}")),
        }
    }

    /// The address after redirects.
    fn final_url(&self) -> Option<String> {
        let mut buf = vec![0u16; 2048];
        let mut len = (buf.len() * 2) as u32; // in bytes
        let ok = unsafe { InternetQueryOptionW(self.handle, INTERNET_OPTION_URL, buf.as_mut_ptr().cast(), &mut len) };
        if ok == 0 {
            return None;
        }
        let chars = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..chars]))
    }

    fn read_all(&self) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            if unsafe { InternetReadFile(self.handle, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read) } == 0 {
                return Err("The download was interrupted".into());
            }
            if read == 0 {
                return Ok(body);
            }
            body.extend_from_slice(&chunk[..read as usize]);
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        unsafe {
            InternetCloseHandle(self.handle);
            InternetCloseHandle(self.session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.4.0"), Some((1, 4, 0)));
        assert_eq!(parse_version("1.10.2"), Some((1, 10, 2)));
        assert_eq!(parse_version("v2"), None);
        assert!(parse_version("v1.10.0") > parse_version("v1.9.9"));
    }

    #[test]
    fn newer_than_this_build() {
        assert!(Release::from_tag("v99.0.0").is_newer());
        assert!(!Release::from_tag(&format!("v{}", current_version())).is_newer());
        assert!(!Release::from_tag("v0.1.0").is_newer());
        assert!(!Release::from_tag("nonsense").is_newer());
    }

    #[test]
    fn tag_from_the_redirect() {
        assert_eq!(tag_from_url("https://github.com/a/b/releases/tag/v1.4.1"), Some("v1.4.1"));
        assert_eq!(tag_from_url("https://github.com/a/b/releases/tag/v1.4.1?x=1"), Some("v1.4.1"));
        // No release yet: GitHub stays on the releases list.
        assert_eq!(tag_from_url("https://github.com/a/b/releases"), None);
    }

    #[test]
    fn addresses_match_the_release_workflow() {
        let release = Release::from_tag("v1.5.0");
        let name = asset_name("v1.5.0");
        assert!(name == "proxy-gui-v1.5.0-x86_64-pc-windows-msvc.exe" || name == "proxy-gui-v1.5.0-aarch64-pc-windows-msvc.exe");
        assert_eq!(release.download, format!("{REPO}/releases/download/v1.5.0/{name}"));
        assert_eq!(release.page, format!("{REPO}/releases/tag/v1.5.0"));
        assert_eq!(sibling(Path::new(r"C:\x\proxy-gui.exe"), "old"), PathBuf::from(r"C:\x\proxy-gui.exe.old"));
    }

    #[test]
    fn versions_in_file_names() {
        assert_eq!(version_in_name("proxy-gui-v1.4.2-x86_64-pc-windows-msvc.exe").as_deref(), Some("v1.4.2"));
        assert_eq!(version_in_name("proxy-gui.exe"), None);
        assert_eq!(version_in_name("proxy-gui-old.exe"), None);
        assert!(is_program_name(Path::new(r"D:\x\proxy-gui-v1.4.0-x86_64-pc-windows-msvc.exe")));
        assert!(is_program_name(Path::new(r"D:\x\Proxy-GUI.exe")));
        assert!(!is_program_name(Path::new(r"D:\x\notes.txt")));
        assert!(!is_program_name(Path::new(r"D:\x\rust-dispatch-proxy.exe")));
    }
}
