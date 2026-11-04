//! Checking GitHub for a newer release, and updating proxy-gui.exe in place.
//!
//! HTTPS goes through WinINet (part of Windows, and it follows the Windows
//! proxy setting), so no extra libraries are needed.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use windows_sys::Win32::Networking::WinInet::{
    HTTP_QUERY_FLAG_NUMBER, HTTP_QUERY_STATUS_CODE, HttpQueryInfoW, INTERNET_FLAG_NO_CACHE_WRITE, INTERNET_FLAG_NO_UI, INTERNET_FLAG_RELOAD,
    INTERNET_OPEN_TYPE_PRECONFIG, InternetCloseHandle, InternetOpenUrlW, InternetOpenW, InternetReadFile,
};

const LATEST: &str = "https://api.github.com/repos/ahmedanjum30/rust-dispatch-proxy/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/ahmedanjum30/rust-dispatch-proxy/releases/latest";

/// The newest release on GitHub.
#[derive(Clone)]
pub struct Release {
    pub tag: String,
    pub page: String,
    /// This PC's proxy-gui download, if the release has one.
    pub download: Option<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
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
}

/// The release asset name for this build, e.g.
/// proxy-gui-v1.5.0-x86_64-pc-windows-msvc.exe.
fn asset_name(tag: &str) -> String {
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    format!("proxy-gui-{tag}-{arch}-pc-windows-msvc.exe")
}

/// Asks GitHub for the latest release.
pub fn latest() -> Result<Release, String> {
    let body = http_get(LATEST, "Accept: application/vnd.github+json\r\n")?;
    let api: ApiRelease = serde_json::from_slice(&body).map_err(|e| format!("Unexpected reply from GitHub: {e}"))?;
    let wanted = asset_name(&api.tag_name);
    let download = api.assets.into_iter().find(|a| a.name == wanted).map(|a| a.browser_download_url);
    Ok(Release {
        tag: api.tag_name,
        page: api.html_url,
        download,
    })
}

/// Downloads the release's exe and puts it in place of this program. The
/// running program keeps working from its old file (renamed to *.old,
/// removed on the next start); the caller then starts the new one.
pub fn install(release: &Release) -> Result<(), String> {
    let url = release.download.as_deref().ok_or("This release has no download for this PC")?;
    let bytes = http_get(url, "")?;
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

/// A simple HTTPS GET; follows redirects. Returns the body of a 200 reply.
fn http_get(url: &str, headers: &str) -> Result<Vec<u8>, String> {
    let agent = wide(&format!("rust-dispatch-proxy/{}", current_version()));
    let (url_w, headers_w) = (wide(url), wide(headers));
    unsafe {
        let session = InternetOpenW(agent.as_ptr(), INTERNET_OPEN_TYPE_PRECONFIG, std::ptr::null(), std::ptr::null(), 0);
        if session.is_null() {
            return Err("Could not start an internet connection".into());
        }
        let flags = INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE | INTERNET_FLAG_NO_UI;
        let header_len = if headers.is_empty() { 0 } else { u32::MAX }; // MAX: zero-terminated
        let request = InternetOpenUrlW(session, url_w.as_ptr(), headers_w.as_ptr(), header_len, flags, 0);
        if request.is_null() {
            InternetCloseHandle(session);
            return Err("Could not reach GitHub. Check your internet connection.".into());
        }
        let mut status = 0u32;
        let mut len = 4u32;
        HttpQueryInfoW(
            request,
            HTTP_QUERY_STATUS_CODE | HTTP_QUERY_FLAG_NUMBER,
            (&mut status as *mut u32).cast(),
            &mut len,
            std::ptr::null_mut(),
        );
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            if InternetReadFile(request, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read) == 0 {
                break;
            }
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
        }
        InternetCloseHandle(request);
        InternetCloseHandle(session);
        if status == 200 {
            Ok(body)
        } else {
            Err(format!("GitHub answered with status {status}"))
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
        let release = |tag: &str| Release {
            tag: tag.into(),
            page: String::new(),
            download: None,
        };
        assert!(release("v99.0.0").is_newer());
        assert!(!release(&format!("v{}", current_version())).is_newer());
        assert!(!release("v0.1.0").is_newer());
        assert!(!release("nonsense").is_newer());
    }

    #[test]
    fn asset_names_match_the_release_workflow() {
        let name = asset_name("v1.5.0");
        assert!(name == "proxy-gui-v1.5.0-x86_64-pc-windows-msvc.exe" || name == "proxy-gui-v1.5.0-aarch64-pc-windows-msvc.exe");
        assert_eq!(sibling(Path::new(r"C:\x\proxy-gui.exe"), "old"), PathBuf::from(r"C:\x\proxy-gui.exe.old"));
    }
}
