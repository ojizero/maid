//! Facts about the host: paths, free space, time, terminal.

use std::ffi::CString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

/// Tests set this to measure free space on a folder in their sandbox.
const VOLUME_ENV: &str = "MAID_TEST_VOLUME";

pub fn home() -> Result<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)),
        _ => bail!("HOME is not set"),
    }
}

/// `~` or `~/x` becomes a path under HOME. Other paths do not change.
pub fn expand(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_path_buf();
    }
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

/// A path under HOME is shown as `~/...`.
pub fn tilde(path: &str, home: &Path) -> String {
    match Path::new(path).strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.to_string(),
    }
}

/// An XDG base folder: `$VAR` if it is an absolute path, else `~/<fallback>`.
pub fn xdg(var: &str, fallback: &str, home: &Path) -> PathBuf {
    match std::env::var_os(var) {
        Some(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => home.join(fallback),
    }
}

/// The volume that holds user data. On macOS `/` is the sealed system volume.
fn volume() -> PathBuf {
    if let Some(path) = std::env::var_os(VOLUME_ENV) {
        return PathBuf::from(path);
    }
    let data = Path::new("/System/Volumes/Data");
    if cfg!(target_os = "macos") && data.exists() {
        data.into()
    } else {
        "/".into()
    }
}

/// Bytes free for an unprivileged user on the data volume.
pub fn free_bytes() -> Result<u64> {
    let path = volume();
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())?;
    // SAFETY: statvfs fills the struct we pass; we read it only on success.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut st) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("statvfs {}", path.display()));
    }
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return "unknown".into();
    }
    let Ok(name) = std::ffi::CStr::from_bytes_until_nul(&buf) else {
        return "unknown".into();
    };
    let name = name.to_string_lossy();
    name.strip_suffix(".local").unwrap_or(&name).to_string()
}

/// The current UTC time as RFC 3339, for example `2026-11-03T09:15:00Z`.
pub fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    rfc3339(secs)
}

fn rfc3339(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Color only on a terminal, and never when NO_COLOR is set.
pub fn color() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_793_700_000), "2026-11-03T10:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn expands_tilde() {
        let home = Path::new("/h");
        assert_eq!(expand("~", home), PathBuf::from("/h"));
        assert_eq!(expand("~/a/b", home), PathBuf::from("/h/a/b"));
        assert_eq!(expand("~x/a", home), PathBuf::from("~x/a"));
        assert_eq!(expand("/abs", home), PathBuf::from("/abs"));
        assert_eq!(tilde("/h/a", home), "~/a");
        assert_eq!(tilde("/other", home), "/other");
    }
}
