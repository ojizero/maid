//! End-to-end tests: the real maid binary in a sandbox, with real git and wt,
//! and fake package tools. See tests/support/mod.rs for the sandbox.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::json;
use support::{Sandbox, is_empty_dir, item_at, item_named, path};

/// The calls that every survey makes when Docker does not run and no
/// program runs: size commands, only_if and skip_if_running checks.
fn survey_calls() -> Vec<String> {
    [
        r#"docker system df --format {{if eq .Type "Build Cache"}}{{.Size}}{{end}}"#,
        r#"docker system df --format {{if eq .Type "Containers"}}{{.Size}}{{end}}"#,
        r#"docker system df --format {{if eq .Type "Images"}}{{.Size}}{{end}}"#,
        r#"docker system df --format {{if eq .Type "Local Volumes"}}{{.Size}}{{end}}"#,
        "docker info",
        "docker info",
        "pgrep -x brew",
        "pgrep -x npm",
        "pgrep -x uv",
    ]
    .map(str::to_string)
    .to_vec()
}

fn strings(calls: &[&str]) -> impl Iterator<Item = String> {
    calls.iter().map(|c| c.to_string())
}

#[test]
fn cleanup_removes_only_what_is_safe() {
    let sb = Sandbox::new("cleanup");
    if !sb.has_wt() {
        return;
    }
    let repo = sb.make_repo();
    let main = path(&repo.main);

    let out = sb.maid(&["cleanup", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();

    // Removed: worktrees 1 and 2. Only the branch that a remote contains is deleted.
    assert!(!repo.pushed.exists());
    assert!(!repo.local.exists());
    assert!(!sb.branch_exists(&repo.main, "pushed"));
    assert!(sb.branch_exists(&repo.main, "local"));

    // Kept: worktrees 3 to 7, with their files.
    assert_eq!(
        fs::read_to_string(repo.dirty.join("README.md")).unwrap(),
        "proj\nchanged\n"
    );
    assert!(repo.env_file.join(".env").is_file());
    assert!(repo.notes.join("tmp/notes.md").is_file());
    assert!(repo.unmerged.join("unmerged.txt").is_file());
    assert!(repo.t3.join("t3.txt").is_file());
    for branch in ["dirty", "env-file", "notes", "unmerged", "t3"] {
        assert!(sb.branch_exists(&repo.main, branch), "{branch}");
    }

    // Emptied by the fakes.
    for cache in ["Library/Caches/Homebrew", ".cache/uv", ".npm/_cacache"] {
        assert!(is_empty_dir(&sb.home.join(cache)), "{cache}");
    }

    // Exactly these commands ran, and nothing else.
    let remove = |p: &Path| {
        format!(
            "wt -C {main} remove --foreground --yes --no-delete-branch {}",
            path(p)
        )
    };
    let list = format!("wt -C {main} list --format=json");
    let mut expected = survey_calls();
    // skip_if_running is checked again just before each clean.
    expected.extend(strings(&[
        "pgrep -x brew",
        "brew cleanup --prune=all",
        "pgrep -x npm",
        "npm cache clean --force",
        "pgrep -x uv",
        "uv cache clean",
        // wt remove looks for a git fsmonitor daemon.
        "pgrep -f git fsmonitor--daemon",
        "pgrep -f git fsmonitor--daemon",
    ]));
    // One list in the survey, then a fresh list before each removal.
    expected.extend([list.clone(), list.clone(), list]);
    expected.extend([remove(&repo.pushed), remove(&repo.local)]);
    sb.assert_calls(expected);

    // The run.
    assert_eq!(run["mode"], "cleanup");
    assert_eq!(run["warnings"], json!([]));
    assert!(run["free_bytes_before"].as_u64().is_some());
    assert_eq!(sb.run_file(), run);

    let pushed = item_at(&run, &repo.pushed);
    // The commands as maid ran them, with quoted values.
    let removed = |p: &Path| {
        format!(
            "wt -C '{main}' remove --foreground --yes --no-delete-branch '{}'",
            path(p)
        )
    };
    assert_eq!(pushed["decision"], "clean");
    assert_eq!(pushed["result"], "done");
    assert_eq!(pushed["branch"], "pushed");
    assert_eq!(
        pushed["commands"],
        json!([
            removed(&repo.pushed),
            format!("git -C '{main}' branch -D 'pushed'")
        ])
    );
    assert!(pushed["freed_bytes"].as_u64().unwrap() > 0);

    let local = item_at(&run, &repo.local);
    assert_eq!(local["result"], "done");
    assert_eq!(local["commands"], json!([removed(&repo.local)]));

    let ask = |p: &Path| {
        let item = item_at(&run, p);
        assert_eq!(item["decision"], "ask", "{item}");
        assert_eq!(item["result"], "skipped", "{item}");
        item["reason"].as_str().unwrap().to_string()
    };
    // The reason for the dirty worktree is checked in
    // dirty_worktree_reason_names_the_changes.
    ask(&repo.dirty);
    assert_eq!(
        ask(&repo.env_file),
        "ignored file is not build output: .env"
    );
    assert_eq!(ask(&repo.notes), "ignored file is not build output: tmp/");
    assert_eq!(ask(&repo.unmerged), "not integrated (state: ahead)");

    let t3 = item_at(&run, &repo.t3);
    assert_eq!(t3["name"], "T3");
    assert_eq!(t3["decision"], "keep");
    assert_eq!(t3["result"], "skipped");

    for name in ["Homebrew cache", "uv cache", "npm cache"] {
        let item = item_named(&run, name);
        assert_eq!(item["result"], "done", "{item}");
        assert!(item["freed_bytes"].as_u64().unwrap() > 0, "{item}");
    }
    for name in ["Docker images", "Docker build cache"] {
        let item = item_named(&run, name);
        assert_eq!(item["result"], "skipped", "{item}");
        assert_eq!(item["reason"], "only_if failed: docker info", "{item}");
    }
    let pnpm = item_named(&run, "pnpm store");
    assert_eq!(pnpm["reason"], "path does not exist");
    assert_eq!(pnpm["result"], "skipped");

    let unknown = run["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "unknown")
        .count();
    assert_eq!(unknown, 0);
}

#[test]
fn dry_run_changes_nothing() {
    let sb = Sandbox::new("dry-run");
    if !sb.has_wt() {
        return;
    }
    let repo = sb.make_repo();
    let before = sb.snapshot();

    let out = sb.maid(&["cleanup", "--dry-run", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    assert_eq!(sb.snapshot(), before);

    let mut expected = survey_calls();
    expected.extend([
        "brew cleanup --prune=all --dry-run".into(),
        format!("wt -C {} list --format=json", path(&repo.main)),
    ]);
    sb.assert_calls(expected);

    let run = out.json();
    assert_eq!(run["mode"], "dry-run");
    assert_eq!(item_at(&run, &repo.pushed)["result"], "planned");
    assert_eq!(item_named(&run, "Homebrew cache")["result"], "planned");
    assert_eq!(sb.run_file(), run);
}

#[test]
fn survey_writes_the_run_file_and_report_prints_it() {
    let sb = Sandbox::new("survey");
    if !sb.has_wt() {
        return;
    }
    let repo = sb.make_repo();
    let before = sb.snapshot();

    let out = sb.maid(&["survey", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    assert_eq!(sb.snapshot(), before);
    let run = out.json();
    assert_eq!(run["mode"], "survey");
    assert_eq!(item_at(&run, &repo.pushed)["result"], "planned");
    assert_eq!(item_at(&run, &repo.dirty)["decision"], "ask");
    assert_eq!(sb.run_file(), run);

    let mut expected = survey_calls();
    expected.push(format!("wt -C {} list --format=json", path(&repo.main)));
    sb.assert_calls(expected);

    let report = sb.maid(&["report", "--format", "json"]);
    assert_eq!(report.code, 0, "{report}");
    assert_eq!(report.json(), run);

    let text = sb.maid(&["survey"]);
    assert_eq!(text.code, 0, "{text}");
    assert!(text.stdout.starts_with("maid survey on "), "{text}");
    assert!(text.stdout.contains("CLEAN · planned"), "{text}");
    assert!(text.stdout.contains("ASK · skipped"), "{text}");
    assert!(text.stdout.contains("~/workspace/proj.dirty"), "{text}");
    assert!(!text.stdout.contains('\x1b'), "{text}");

    let report = sb.maid(&["report"]);
    assert_eq!(report.code, 0, "{report}");
    assert!(!report.stdout.contains('\x1b'), "{report}");
}

/// wt reports a merged branch with changes as "behind", not "integrated".
/// The reason must name the changes, not the state.
#[test]
fn dirty_worktree_reason_names_the_changes() {
    let sb = Sandbox::new("dirty-reason");
    if !sb.has_wt() {
        return;
    }
    let repo = sb.make_repo();
    let out = sb.maid(&["survey", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();
    let dirty = item_at(&run, &repo.dirty);
    assert_eq!(dirty["decision"], "ask");
    assert_eq!(dirty["reason"], "has changes: modified");
}

/// A branch at the same commit as main has wt state "empty". It can be a
/// worktree made a moment ago for new work, so maid asks.
#[test]
fn worktree_at_main_goes_to_ask() {
    let sb = Sandbox::new("empty");
    if !sb.has_wt() {
        return;
    }
    let repo = sb.make_repo();
    let fresh = repo.main.with_file_name("proj.fresh");
    sb.git(
        &repo.main,
        &["worktree", "add", "--quiet", "-b", "fresh", path(&fresh)],
    );

    let out = sb.maid(&["survey", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();
    let item = item_at(&run, &fresh);
    assert_eq!(item["decision"], "ask", "{item}");
    assert_eq!(item["reason"], "not integrated (state: empty)");
}

#[test]
fn only_cleans_the_named_target() {
    let sb = Sandbox::new("only");
    if !sb.has_wt() {
        return;
    }
    let repo = sb.make_repo();

    let out = sb.maid(&["cleanup", "--only", "uv cache", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();

    assert!(is_empty_dir(&sb.home.join(".cache/uv")));
    assert!(!is_empty_dir(&sb.home.join("Library/Caches/Homebrew")));
    assert!(!is_empty_dir(&sb.home.join(".npm/_cacache")));
    assert!(repo.pushed.exists());
    assert!(repo.local.exists());

    assert_eq!(item_named(&run, "uv cache")["result"], "done");
    for item in [
        item_named(&run, "Homebrew cache"),
        item_named(&run, "npm cache"),
        item_at(&run, &repo.pushed),
    ] {
        assert_eq!(item["result"], "skipped", "{item}");
        assert_eq!(item["reason"], "not selected by --only", "{item}");
    }

    let mut expected = survey_calls();
    expected.extend(strings(&["pgrep -x uv", "uv cache clean"]));
    expected.push(format!("wt -C {} list --format=json", path(&repo.main)));
    sb.assert_calls(expected);
}

#[test]
fn failed_clean_is_reported_and_others_still_run() {
    let sb = Sandbox::new("fail");
    sb.set("fail", &["uv"]);

    let out = sb.maid(&["cleanup", "--format", "json"]);
    assert_eq!(out.code, 1, "{out}");
    let run = out.json();

    let uv = item_named(&run, "uv cache");
    assert_eq!(uv["result"], "failed");
    assert_eq!(uv["error"], "uv: first error line\nuv: simulated failure");
    assert!(!is_empty_dir(&sb.home.join(".cache/uv")));
    for name in ["Homebrew cache", "npm cache"] {
        assert_eq!(item_named(&run, name)["result"], "done", "{name}");
    }
    assert!(is_empty_dir(&sb.home.join("Library/Caches/Homebrew")));
    assert_eq!(sb.run_file(), run);
}

#[test]
fn skip_if_running_skips_the_target() {
    let sb = Sandbox::new("running");
    sb.set("running", &["brew"]);

    let out = sb.maid(&["cleanup", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();

    let brew = item_named(&run, "Homebrew cache");
    assert_eq!(brew["decision"], "clean");
    assert_eq!(brew["result"], "skipped");
    assert_eq!(brew["reason"], "brew is running");
    assert!(!is_empty_dir(&sb.home.join("Library/Caches/Homebrew")));
    assert_eq!(item_named(&run, "uv cache")["result"], "done");

    let calls = sb.calls();
    assert!(!calls.iter().any(|c| c.starts_with("brew")), "{calls:?}");
    assert!(calls.contains(&"uv cache clean".to_string()), "{calls:?}");
}

#[test]
fn pgrep_failure_skips_the_target() {
    let sb = Sandbox::new("pgrep-broken");
    fs::write(sb.root.join("pgrep-broken"), "").unwrap();

    let out = sb.maid(&["cleanup", "--only", "uv cache", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let uv = item_named(&out.json(), "uv cache").clone();
    assert_eq!(uv["result"], "skipped", "{uv}");
    assert!(
        uv["reason"]
            .as_str()
            .unwrap()
            .starts_with("cannot check if uv is running")
    );
    assert!(!is_empty_dir(&sb.home.join(".cache/uv")));
    assert!(!sb.calls().contains(&"uv cache clean".to_string()));
}

#[test]
fn symlinked_target_path_is_not_cleaned() {
    let sb = Sandbox::new("symlink");
    let elsewhere = sb.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("keep"), "data").unwrap();
    let derived = sb.home.join("Library/Developer/Xcode/DerivedData");
    fs::create_dir_all(derived.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &derived).unwrap();

    let out = sb.maid(&["cleanup", "--only", "Xcode DerivedData", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let item = item_named(&out.json(), "Xcode DerivedData").clone();
    assert_eq!(item["result"], "skipped", "{item}");
    assert_eq!(item["reason"], "path is a symlink");
    assert!(elsewhere.join("keep").is_file());
}

#[test]
fn failed_dry_run_command_exits_1() {
    let sb = Sandbox::new("dry-run-fail");
    sb.set("fail", &["brew"]);

    let out = sb.maid(&["cleanup", "--dry-run", "--format", "json"]);
    assert_eq!(out.code, 1, "{out}");
    let brew = item_named(&out.json(), "Homebrew cache").clone();
    assert_eq!(brew["result"], "failed", "{brew}");
    assert!(
        brew["error"]
            .as_str()
            .unwrap()
            .contains("simulated failure"),
        "{brew}"
    );
    assert!(!is_empty_dir(&sb.home.join("Library/Caches/Homebrew")));
}

#[test]
fn docker_targets_clean_only_when_docker_runs() {
    let sb = Sandbox::new("docker");
    fs::write(sb.root.join("docker-running"), "").unwrap();

    let out = sb.maid(&["cleanup", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();

    for name in ["Docker images", "Docker build cache"] {
        let item = item_named(&run, name);
        assert_eq!(item["size_bytes"], 1_500_000_000, "{item}");
        assert_eq!(item["result"], "done", "{item}");
    }
    assert_eq!(item_named(&run, "Docker volumes")["decision"], "keep");

    let prunes: Vec<String> = sb
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("docker") && c.contains("prune"))
        .collect();
    assert_eq!(
        prunes,
        [
            "docker builder prune --all --force",
            "docker image prune --all --force"
        ]
    );
}

#[test]
fn missing_wt_skips_the_repo() {
    let sb = Sandbox::new("no-wt");
    let repo = sb.make_repo();
    let _ = fs::remove_file(sb.root.join("bin/wt"));

    let out = sb.maid(&["survey", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();
    let item = item_named(&run, "wt list");
    assert_eq!(item["repo"], path(&repo.main));
    assert_eq!(item["decision"], "ask");
    assert_eq!(item["result"], "skipped");
    let reason = item["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("repo skipped: cannot start wt"),
        "{reason}"
    );
}

// ── Errors ───────────────────────────────────────────────────────

fn assert_fails(out: &support::MaidOutput, code: i32, message: &str) {
    assert_eq!(out.code, code, "{out}");
    assert!(out.stderr.contains(message), "{out}");
}

#[test]
fn unknown_config_key_is_a_usage_error() {
    let sb = Sandbox::new("bad-key");
    sb.write_config(
        "[[target]]\nname = \"x\"\naction = \"keep\"\npath = \"~/x\"\nwhy = \"y\"\ncolour = \"red\"\n",
    );
    assert_fails(&sb.maid(&["survey"]), 2, "unknown field `colour`");
    assert!(sb.calls().is_empty());
}

#[test]
fn writable_config_is_refused() {
    let sb = Sandbox::new("writable");
    let file = sb.write_config("");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o620)).unwrap();
    assert_fails(
        &sb.maid(&["survey"]),
        2,
        "must be owned by you and not writable by group or others",
    );
}

#[test]
fn missing_config_flag_file_is_a_usage_error() {
    let sb = Sandbox::new("missing-config");
    let missing = sb.root.join("nope.toml");
    assert_fails(
        &sb.maid(&["-c", path(&missing), "survey"]),
        2,
        "cannot read config",
    );
}

#[test]
fn unknown_only_name_is_a_usage_error() {
    let sb = Sandbox::new("bad-only");
    assert_fails(
        &sb.maid(&["cleanup", "--only", "nope"]),
        2,
        "--only \"nope\" matches no target or override",
    );
    assert!(sb.calls().is_empty());
}

#[test]
fn report_errors() {
    let sb = Sandbox::new("report");
    assert_fails(
        &sb.maid(&["report", "--format", "html"]),
        2,
        "html output is not implemented yet",
    );
    assert_fails(&sb.maid(&["report"]), 1, "run `maid survey` first");
}

#[test]
fn user_config_adds_a_target() {
    let sb = Sandbox::new("user-target");
    let cache = sb.home.join("scratch");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("f"), "x").unwrap();
    sb.write_config(
        "[[target]]\nname = \"Scratch\"\naction = \"clean\"\npath = \"~/scratch\"\n\
         clean = \"purge scratch\"\n",
    );

    let out = sb.maid(&["cleanup", "--only", "Scratch", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();
    let item = item_named(&run, "Scratch");
    assert_eq!(item["result"], "done");
    assert_eq!(item["commands"], json!(["purge scratch"]));
    assert!(sb.calls().contains(&"purge scratch".to_string()));
}

#[test]
#[should_panic(expected = "refusing to run maid")]
fn harness_refuses_a_home_outside_the_temp_folder() {
    let mut sb = Sandbox::new("guard");
    sb.home = "/".into();
    // report only reads the run file, in case the guard ever fails.
    sb.maid(&["report"]);
}
