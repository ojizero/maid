//! Run external commands with a timeout and captured output.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use wait_timeout::ChildExt;

/// The longest time one command can run.
pub const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The number of stderr lines kept when a command fails.
const TAIL_LINES: usize = 5;

pub struct Output {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// A short error text: the last stderr lines, or stdout lines when
    /// stderr is empty.
    pub fn failure(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        tail(text, TAIL_LINES)
    }
}

pub fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Run `cmd` with `sh -c`.
pub fn sh(cmd: &str) -> Result<Output> {
    run(&["sh", "-c", cmd])
}

/// Run a program with arguments. The program is found on PATH.
/// An error means the program did not start or did not stop in time.
/// A non-zero exit is not an error: see `Output::status`.
pub fn run(argv: &[&str]) -> Result<Output> {
    run_with_timeout(argv, TIMEOUT)
}

fn run_with_timeout(argv: &[&str], timeout: Duration) -> Result<Output> {
    let (mut stdout, mut stderr) = (scratch_file()?, scratch_file()?);
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        // git status must not refresh the index: a survey changes nothing.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        // Own process group, so a timeout stops the command and its children.
        .process_group(0)
        .spawn()
        .with_context(|| format!("cannot start {}", argv[0]))?;

    let Some(status) = child.wait_timeout(timeout)? else {
        stop_group(&mut child);
        bail!(
            "{} did not finish in {} s and was stopped",
            argv.join(" "),
            timeout.as_secs()
        );
    };
    Ok(Output {
        status,
        stdout: read_all(&mut stdout)?,
        stderr: read_all(&mut stderr)?,
    })
}

/// A file with no name, for the output of one command. It is a file and
/// not a pipe, so a process that the command started in the background
/// and that keeps it open cannot make maid wait.
fn scratch_file() -> Result<File> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("maid-{}-{n}", std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    std::fs::remove_file(&path)?;
    Ok(file)
}

fn read_all(file: &mut File) -> Result<String> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Stop the process group that maid started for one command: SIGTERM,
/// then SIGKILL after 5 seconds. The group gets SIGKILL even when the
/// command itself stopped, so its children cannot stay behind.
fn stop_group(child: &mut std::process::Child) {
    let pgid = -(child.id() as i32);
    // SAFETY: kill(2) with a negative pid signals only our child's group.
    unsafe { libc::kill(pgid, libc::SIGTERM) };
    let _ = child.wait_timeout(Duration::from_secs(5));
    unsafe { libc::kill(pgid, libc::SIGKILL) };
    let _ = child.wait();
}

/// Quote a value for `sh`.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn quote_survives_the_shell() {
        for value in ["plain", "it's", "a b", "$(touch x)", "`x`", "\"q\"", ""] {
            let out = sh(&format!("printf %s {}", quote(value))).unwrap();
            assert_eq!(out.stdout, value);
        }
    }

    #[test]
    fn timeout_stops_the_command() {
        let err = run_with_timeout(&["sh", "-c", "sleep 30"], Duration::from_millis(200));
        assert!(err.err().unwrap().to_string().contains("did not finish"));
    }

    #[test]
    fn background_process_cannot_hold_output() {
        let start = Instant::now();
        // The sleep outlives the test, so keep it short.
        let out = sh("echo hi; sleep 3 &").unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, "hi\n");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn failure_keeps_last_stderr_lines() {
        let out = sh("for i in 1 2 3 4 5 6 7; do echo line$i >&2; done; exit 3").unwrap();
        assert!(!out.status.success());
        assert_eq!(out.failure(), "line3\nline4\nline5\nline6\nline7");
    }
}
