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

/// Downloads the release's exe and puts it in place of this program. The
/// running program keeps working from its old file (renamed to *.old,
/// removed on the next start); the caller then starts the new one.
pub fn install(release: &Release) -> Result<(), String> {
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
    let exe = std::env::current_exe().map_err(|e| format!("Could not locate this program: {e}"))?;
    let (new, old) = (sibling(&exe, "new"), sibling(&exe, "old"));
    std::fs::write(&new, &bytes).map_err(|e| format!("Could not save the update next to this program: {e}"))?;
    let _ = std::fs::remove_file(&old);
    // Windows lets a running program's file be renamed, but not replaced.
    std::fs::rename(&exe, &old).map_err(|e| {
        let _ = std::fs::remove_file(&new);
        format!("Could not replace this program: {e}")
    })?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe); // put the old one back
        return Err(format!("Could not replace this program: {e}"));
    }
    Ok(())
}

/// Removes the previous version left behind by an update. Right after an
/// update the old program may still be closing, so this keeps trying for a
/// few seconds in the background.
pub fn clean_up_old() {
    let Ok(exe) = std::env::current_exe() else { return };
    let old = sibling(&exe, "old");
    if !old.exists() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..20 {
            if std::fs::remove_file(&old).is_ok() || !old.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
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
}
