//! Log output: timestamped lines on stderr, or in a file with -log.

use std::fmt::Arguments;
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Mutex, OnceLock, PoisonError};

static FILE: OnceLock<Mutex<File>> = OnceLock::new();

/// Sends log lines to `path` from now on. The file starts empty each run.
pub(crate) fn to_file(path: &str) -> io::Result<()> {
    let file = File::create(path)?;
    let _ = FILE.set(Mutex::new(file));
    Ok(())
}

pub(crate) fn emit(level: &str, msg: Arguments) {
    let line = format!("{} [{level}] {msg}\n", timestamp());
    match FILE.get() {
        Some(file) => {
            let _ = file.lock().unwrap_or_else(PoisonError::into_inner).write_all(line.as_bytes());
        }
        None => {
            let _ = io::stderr().write_all(line.as_bytes());
        }
    }
}

/// Local time on Windows; UTC elsewhere (std has no time zones).
fn timestamp() -> String {
    #[cfg(windows)]
    {
        let mut t: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
        )
    }
    #[cfg(not(windows))]
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let (y, m, d) = civil_from_days((secs / 86_400) as i64);
        let s = secs % 86_400;
        format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
    }
}

/// Days since 1970-01-01 to (year, month, day), from Howard Hinnant's
/// public-domain date algorithms.
#[cfg_attr(windows, allow(dead_code))]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_738), (2026, 10, 12));
    }
}
