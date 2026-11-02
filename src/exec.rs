//! Run external commands with a timeout and captured output.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use wait_timeout::ChildExt;

/// The longest time one command can run.
pub const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The number of stderr lines kept when a command fails.
const TAIL_LINES: usize = 5;

pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// A short error text: the exit status and the last stderr lines.
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
/// A non-zero exit is not an error: see `Output::success`.
pub fn run(argv: &[&str]) -> Result<Output> {
    run_with_timeout(argv, TIMEOUT)
}

pub fn run_with_timeout(argv: &[&str], timeout: Duration) -> Result<Output> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout stops the command and its children.
        .process_group(0)
        .spawn()
        .with_context(|| format!("cannot start {}", argv[0]))?;

    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());

    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            stop_group(&mut child);
            bail!(
                "{} did not finish in {} s and was stopped",
                argv.join(" "),
                timeout.as_secs()
            );
        }
    };
    Ok(Output {
        success: status.success(),
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Stop the process group that maid started for one command:
/// SIGTERM, then SIGKILL if it is still there after 5 seconds.
fn stop_group(child: &mut std::process::Child) {
    let pgid = -(child.id() as i32);
    // SAFETY: kill(2) with a negative pid signals only our child's group.
    unsafe { libc::kill(pgid, libc::SIGTERM) };
    if !matches!(child.wait_timeout(Duration::from_secs(5)), Ok(Some(_))) {
        unsafe { libc::kill(pgid, libc::SIGKILL) };
        let _ = child.wait();
    }
}

/// Quote a value for `sh`.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_survives_the_shell() {
        for value in ["plain", "it's", "a b", "$(rm -rf /)", "`x`", "\"q\"", ""] {
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
    fn failure_keeps_last_stderr_lines() {
        let out = sh("for i in 1 2 3 4 5 6 7; do echo line$i >&2; done; exit 3").unwrap();
        assert!(!out.success);
        assert_eq!(out.failure(), "line3\nline4\nline5\nline6\nline7");
    }
}
