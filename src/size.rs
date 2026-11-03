//! Parse and format sizes.

use std::path::Path;

use anyhow::{Result, bail};

use crate::exec;

pub const KIB: u64 = 1024;
pub const MIB: u64 = 1024 * KIB;
pub const GIB: u64 = 1024 * MIB;

/// Parse a size such as `17.3GB` as Docker prints it (decimal SI units).
/// Binary units (`KiB`, `MiB`, ...) are accepted too.
/// Empty output means the size is not known.
pub fn parse(text: &str) -> Result<Option<u64>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let Ok(number) = number.parse::<f64>() else {
        bail!("not a size: {text:?}");
    };
    let factor: f64 = match unit.trim() {
        "" | "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "PB" => 1e15,
        "KiB" => KIB as f64,
        "MiB" => MIB as f64,
        "GiB" => GIB as f64,
        "TiB" => (GIB * 1024) as f64,
        _ => bail!("not a size: {text:?}"),
    };
    Ok(Some((number * factor).round() as u64))
}

/// Format bytes in GiB, or in MiB below 1 GiB.
pub fn human(bytes: u64) -> String {
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{:.0} MiB", bytes as f64 / MIB as f64)
    }
}

/// The size of a folder in bytes, from `du -skx`. None when it does not exist.
pub fn du(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path).ok()?;
    // -x: stay on this volume, so mounted network or removable volumes
    // are not read (macOS asks for permission to read them).
    let out = exec::run(&["du", "-skx", &path.to_string_lossy()]).ok()?;
    // du exits non-zero when some files are unreadable but still prints a total.
    let kib: u64 = out.stdout.split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_sizes() {
        assert_eq!(parse("17.3GB").unwrap(), Some(17_300_000_000));
        assert_eq!(parse("0B").unwrap(), Some(0));
        assert_eq!(parse("1.2kB").unwrap(), Some(1_200));
        assert_eq!(parse("512MB\n").unwrap(), Some(512_000_000));
        assert_eq!(parse("2TB").unwrap(), Some(2_000_000_000_000));
        assert_eq!(parse("1.5GiB").unwrap(), Some(1_610_612_736));
    }

    #[test]
    fn empty_output_is_unknown() {
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("\n\n  \n").unwrap(), None);
    }

    #[test]
    fn rejects_junk() {
        assert!(parse("GB").is_err());
        assert!(parse("12 apples").is_err());
        assert!(parse("Cannot connect to the Docker daemon").is_err());
    }

    #[test]
    fn formats() {
        assert_eq!(human(3 * GIB / 2), "1.5 GiB");
        assert_eq!(human(300 * MIB), "300 MiB");
    }
}
