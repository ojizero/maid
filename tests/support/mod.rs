//! The sandbox for end-to-end tests: a temp folder that holds HOME, the XDG
//! folders, fake tools and throwaway git repos. maid runs only inside it.
//!
//! Layout:
//!   bin/            fake tools and a logging `wt` wrapper, first on PATH
//!   home/           HOME
//!   config/         XDG_CONFIG_HOME; holds the default config
//!   state/          XDG_STATE_HOME (the run files)
//!   tmp/            TMPDIR
//!   remote.git      the bare "remote" of the test repo
//!   gitconfig       GIT_CONFIG_GLOBAL
//!   calls.log       one line per fake tool call: `brew cleanup --prune=all`
//!   running         program names the fake `pgrep` finds
//!   pgrep-broken    when it exists, the fake `pgrep` fails (exit 3)
//!   fail            fake tool names that fail
//!   docker-running  when it exists, the fake `docker` works
//!   mid-run.json    the run file as the fake `uv cache clean` found it
//!
//! Set MAID_KEEP_SANDBOX=1 to keep the folder after the test.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

/// Programs that the default targets call. Each fake logs its call.
const FAKES: &[&str] = &[
    "brew", "bun", "docker", "dotnet", "go", "mise", "npm", "pgrep", "pnpm", "purge", "uv", "xcrun",
];

/// The config that `maid init` writes.
pub const DEFAULTS: &str = include_str!("../../config/default.toml");

/// Real programs come only from these folders, after the fakes.
const SYSTEM_PATH: &str = "/usr/bin:/bin";

pub struct Sandbox {
    pub root: PathBuf,
    pub home: PathBuf,
}

pub struct MaidOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The test repo at ~/workspace/proj and its worktrees, by expected outcome.
pub struct Repo {
    pub main: PathBuf,
    /// 1. Integrated, clean, only build output ignored, branch pushed: removed, branch deleted.
    pub pushed: PathBuf,
    /// 2. Integrated, clean, branch not on the remote: removed, branch kept.
    pub local: PathBuf,
    /// 3. Integrated, a tracked file changed: ask.
    pub dirty: PathBuf,
    /// 4. Integrated, an ignored `.env`: ask.
    pub env_file: PathBuf,
    /// 5. Integrated, an ignored `tmp/notes.md`: ask.
    pub notes: PathBuf,
    /// 6. A commit that main does not have: ask.
    pub unmerged: PathBuf,
    /// 7. Under ~/.t3/worktrees: keep (T3 override).
    pub t3: PathBuf,
}

impl Sandbox {
    pub fn new(test: &str) -> Sandbox {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let name = format!(
            "maid-e2e-{test}-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(name);
        fs::create_dir(&dir).unwrap();
        // git and wt print canonical paths (/private/var/..., not /var/...),
        // and maid matches override patterns against them.
        let root = fs::canonicalize(&dir).unwrap();
        assert!(
            !root.to_string_lossy().contains('\''),
            "the fake tools quote the sandbox path with '"
        );
        let sb = Sandbox {
            home: root.join("home"),
            root,
        };
        for dir in ["bin", "home", "config/maid", "state", "tmp"] {
            fs::create_dir_all(sb.root.join(dir)).unwrap();
        }
        fs::write(
            sb.root.join("gitconfig"),
            "[user]\n\tname = maid test\n\temail = test@example.com\n\
             [init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        sb.install_fakes();
        sb.make_caches();
        sb.write_config(DEFAULTS);
        sb
    }

    fn install_fakes(&self) {
        let fake = format!(
            r#"#!/bin/sh
# A fake tool for maid tests: log the call, then act on the sandbox only.
S='{root}'
H='{home}'
name=$(basename "$0")
echo "$name $*" >> "$S/calls.log"
# Where each tool keeps its cache. These calls do not fail.
case "$name $*" in
  "brew --cache") echo "$H/Library/Caches/Homebrew"; exit 0 ;;
  "uv cache dir") echo "$H/.cache/uv"; exit 0 ;;
  "npm config get cache") echo "$H/.npm"; exit 0 ;;
  "pnpm store path") echo "$H/Library/pnpm/store"; exit 0 ;;
  "go env GOCACHE") echo "$H/Library/Caches/go-build"; exit 0 ;;
esac
if grep -qx "$name" "$S/fail" 2>/dev/null; then
  echo "$name: first error line" >&2
  echo "$name: simulated failure" >&2
  exit 1
fi
case "$name $*" in
  "pgrep -x "*) test -e "$S/pgrep-broken" && exit 3
                grep -qx "$2" "$S/running" 2>/dev/null || exit 1 ;;
  "pgrep "*) exit 1 ;;
  "docker info") test -e "$S/docker-running" ;;
  "docker system df "*) test -e "$S/docker-running" && echo 1.5GB ;;
  "brew cleanup --prune=all") rm -rf "$H/Library/Caches/Homebrew/"* ;;
  "uv cache clean") rm -rf "$H/.cache/uv/"*
                    cp "$S"/state/maid/runs/*.json "$S/mid-run.json" ;;
  "npm cache clean --force") rm -rf "$H/.npm/_cacache/"* ;;
esac
"#,
            root = self.root.display(),
            home = self.home.display(),
        );
        for name in FAKES {
            write_script(&self.root.join("bin").join(name), &fake);
        }
        if let Some(wt) = real_wt() {
            let wrapper = format!(
                "#!/bin/sh\necho \"wt $*\" >> '{}/calls.log'\nexec '{}' \"$@\"\n",
                self.root.display(),
                wt.display()
            );
            write_script(&self.root.join("bin/wt"), &wrapper);
        }
    }

    /// Three caches with files, so the fakes have something to empty.
    /// The other default target paths do not exist.
    fn make_caches(&self) {
        for cache in ["Library/Caches/Homebrew", ".cache/uv", ".npm/_cacache"] {
            let dir = self.home.join(cache);
            fs::create_dir_all(dir.join("sub")).unwrap();
            for file in ["a", "b", "sub/c"] {
                fs::write(dir.join(file), vec![b'x'; 8192]).unwrap();
            }
        }
    }

    /// A command with the sandbox environment and nothing else.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env(
                "PATH",
                format!("{}:{SYSTEM_PATH}", self.root.join("bin").display()),
            )
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("MAID_TEST_VOLUME", &self.root)
            .env("NO_COLOR", "1")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"));
        cmd
    }

    /// Run maid in the sandbox. Panics when HOME is not inside the temp folder.
    pub fn maid(&self, args: &[&str]) -> MaidOutput {
        let tmp = fs::canonicalize(std::env::temp_dir()).unwrap();
        let home = fs::canonicalize(&self.home).unwrap();
        assert!(
            home.starts_with(&tmp) && home != tmp,
            "refusing to run maid: HOME {} is not inside {}",
            home.display(),
            tmp.display()
        );
        let out = self
            .command(env!("CARGO_BIN_EXE_maid"))
            .args(args)
            .output()
            .unwrap();
        MaidOutput {
            code: out.status.code().expect("maid was stopped by a signal"),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    }

    /// True when the real wt is installed. Tests that need it return early
    /// when it is not.
    pub fn has_wt(&self) -> bool {
        let found = self.root.join("bin/wt").exists();
        if !found {
            eprintln!("SKIPPED: wt not found (set MAID_TEST_WT to its path)");
        }
        found
    }

    /// Run git in `dir` and panic when it fails.
    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = self
            .command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    pub fn branch_exists(&self, repo: &Path, branch: &str) -> bool {
        self.command("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{branch}"))
            .output()
            .unwrap()
            .status
            .success()
    }

    /// Make the test repo with a bare remote and the 7 worktrees of `Repo`.
    pub fn make_repo(&self) -> Repo {
        let workspace = self.home.join("workspace");
        let main = workspace.join("proj");
        let remote = self.root.join("remote.git");
        fs::create_dir_all(&main).unwrap();
        self.git(&self.root, &["init", "--quiet", "--bare", path(&remote)]);
        self.git(&main, &["init", "--quiet"]);
        write(
            &main.join(".gitignore"),
            "node_modules/\ntarget/\n.env\ntmp/\n",
        );
        write(&main.join("README.md"), "proj\n");
        self.git(&main, &["add", "."]);
        self.git(&main, &["commit", "--quiet", "-m", "init"]);
        self.git(&main, &["remote", "add", "origin", path(&remote)]);
        // wt writes this to .git/config on its first run; set it now so that
        // a survey leaves the sandbox as it was.
        self.git(&main, &["config", "worktrunk.default-branch", "main"]);
        self.git(&main, &["push", "--quiet", "-u", "origin", "main"]);

        let add = |branch: &str, dir: PathBuf| {
            self.git(
                &main,
                &["worktree", "add", "--quiet", "-b", branch, path(&dir)],
            );
            self.commit(&dir, &format!("{branch}.txt"));
            dir
        };
        // wt says "integrated" only when main has the branch's commits and
        // more: a branch at main is "empty". So each integrated branch has a
        // commit merged into main, and main moves on after the merges.
        let merged = |branch: &str| {
            let dir = add(branch, workspace.join(format!("proj.{branch}")));
            self.git(&main, &["merge", "--quiet", "--ff-only", branch]);
            dir
        };

        let pushed = merged("pushed");
        write(&pushed.join("node_modules/pkg/index.js"), "x\n");
        write(&pushed.join("target/debug/app"), "x\n");
        self.git(&main, &["push", "--quiet", "origin", "main", "pushed"]);

        // Merged after the last push: no remote branch contains these.
        let local = merged("local");
        let dirty = merged("dirty");
        write(&dirty.join("README.md"), "proj\nchanged\n");
        let env_file = merged("env-file");
        write(&env_file.join(".env"), "SECRET=1\n");
        let notes = merged("notes");
        write(&notes.join("tmp/notes.md"), "notes\n");
        self.commit(&main, "main.txt");

        let unmerged = add("unmerged", workspace.join("proj.unmerged"));
        let t3 = self.home.join(".t3/worktrees/proj/t3code-1234");
        add("t3", t3.clone());

        Repo {
            main,
            pushed,
            local,
            dirty,
            env_file,
            notes,
            unmerged,
            t3,
        }
    }

    /// Add a file and commit it.
    pub fn commit(&self, dir: &Path, file: &str) {
        write(&dir.join(file), "work\n");
        self.git(dir, &["add", "."]);
        self.git(dir, &["commit", "--quiet", "-m", file]);
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config/maid/config.toml")
    }

    /// Write the config with mode 0600.
    pub fn write_config(&self, text: &str) -> PathBuf {
        let file = self.config_file();
        fs::write(&file, text).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        file
    }

    /// Add `text` to the end of the default config.
    pub fn add_config(&self, text: &str) {
        self.write_config(&format!("{DEFAULTS}\n{text}"));
    }

    /// Write one name per line to a sandbox file, for example `running`.
    pub fn set(&self, file: &str, names: &[&str]) {
        let lines: String = names.iter().map(|n| format!("{n}\n")).collect();
        fs::write(self.root.join(file), lines).unwrap();
    }

    /// Assert that the fake tools got exactly these calls, in any order.
    pub fn assert_calls(&self, mut expected: Vec<String>) {
        let calls = self.calls();
        expected.sort();
        let missing: Vec<_> = expected.iter().filter(|c| !calls.contains(c)).collect();
        let extra: Vec<_> = calls.iter().filter(|c| !expected.contains(c)).collect();
        assert_eq!(
            calls, expected,
            "\nnot called: {missing:#?}\nnot expected: {extra:#?}"
        );
    }

    /// Every fake tool call, sorted: the order of parallel calls varies.
    pub fn calls(&self) -> Vec<String> {
        let log = fs::read_to_string(self.root.join("calls.log")).unwrap_or_default();
        let mut calls: Vec<String> = log.lines().map(str::to_string).collect();
        calls.sort();
        calls
    }

    /// The run files, oldest first.
    pub fn run_files(&self) -> Vec<PathBuf> {
        let dir = self.root.join("state/maid/runs");
        let mut files: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        files
    }

    /// The newest run file.
    pub fn run_file(&self) -> Value {
        let text = fs::read_to_string(self.run_files().last().unwrap()).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Every path in the sandbox, with the size of each file. It leaves out
    /// what maid and the fakes are expected to write: the run files, the call
    /// log, and the cache that wt keeps in .git/wt.
    pub fn snapshot(&self) -> Vec<(String, u64)> {
        let mut paths = Vec::new();
        walk(&self.root, &mut |p| {
            let rel = p
                .strip_prefix(&self.root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if rel == "calls.log" || rel.starts_with("state") || rel.contains(".git/wt") {
                return;
            }
            let meta = fs::symlink_metadata(p).unwrap();
            paths.push((rel, if meta.is_file() { meta.len() } else { 0 }));
        });
        paths.sort();
        paths
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if std::env::var_os("MAID_KEEP_SANDBOX").is_some() {
            eprintln!("sandbox kept: {}", self.root.display());
        } else {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

impl MaidOutput {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| panic!("{e}\n{self}"))
    }
}

impl fmt::Display for MaidOutput {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "exit code {}\n--- stdout\n{}--- stderr\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

/// The run item with this path (worktrees and path targets).
pub fn item_at<'a>(run: &'a Value, path: &Path) -> &'a Value {
    let path = path.to_str().unwrap();
    find(run, |i| i["path"] == path).unwrap_or_else(|| panic!("no item at {path}"))
}

/// The run item with this name (targets).
pub fn item_named<'a>(run: &'a Value, name: &str) -> &'a Value {
    find(run, |i| i["name"] == name).unwrap_or_else(|| panic!("no item named {name}"))
}

fn find(run: &Value, f: impl Fn(&Value) -> bool) -> Option<&Value> {
    run["items"].as_array().unwrap().iter().find(|i| f(i))
}

pub fn is_empty_dir(dir: &Path) -> bool {
    fs::read_dir(dir).unwrap().next().is_none()
}

pub fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

fn write_script(file: &Path, text: &str) {
    fs::write(file, text).unwrap();
    fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        f(&path);
        if fs::symlink_metadata(&path).unwrap().is_dir() {
            walk(&path, f);
        }
    }
}

/// The real wt: $MAID_TEST_WT, else the first `wt` on PATH that is not a
/// mise shim (a shim may not work with the sandbox HOME), else `mise which wt`.
fn real_wt() -> Option<PathBuf> {
    if let Some(wt) = std::env::var_os("MAID_TEST_WT") {
        return Some(PathBuf::from(wt));
    }
    let on_path = std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| !dir.to_string_lossy().contains("mise/shims"))
        .map(|dir| dir.join("wt"))
        .find(|wt| wt.is_file());
    on_path.or_else(|| {
        let out = Command::new("mise").args(["which", "wt"]).output().ok()?;
        let wt = String::from_utf8(out.stdout).ok()?;
        out.status.success().then(|| PathBuf::from(wt.trim()))
    })
}
