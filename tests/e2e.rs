//! End-to-end tests: the real maid binary in a sandbox, with real git and wt,
//! and fake package tools. See tests/support/mod.rs for the sandbox.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::json;
use support::{DEFAULTS, Sandbox, is_empty_dir, item_at, item_named, path};

/// The calls that every survey makes when Docker does not run and no
/// program runs: size and locate commands, only_if and skip_if_running
/// checks.
fn survey_calls() -> Vec<String> {
    [
        r#"docker system df --format {{if eq .Type "Build Cache"}}{{.Size}}{{end}}"#,
        r#"docker system df --format {{if eq .Type "Containers"}}{{.Size}}{{end}}"#,
        r#"docker system df --format {{if eq .Type "Images"}}{{.Size}}{{end}}"#,
        r#"docker system df --format {{if eq .Type "Local Volumes"}}{{.Size}}{{end}}"#,
        "docker info",
        "docker info",
        "brew --cache",
        "go env GOCACHE",
        "npm config get cache",
        "pnpm store path",
        "uv cache dir",
        "pgrep -x brew",
        "pgrep -x npm",
        "pgrep -x uv",
    ]
    .map(str::to_string)
    .to_vec()
}

#[test]
fn cleanup_removes_only_what_is_safe() {
    let sb = Sandbox::new("cleanup");
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
    expected.extend(
        [
            "pgrep -x brew",
            "brew cleanup --prune=all",
            "pgrep -x npm",
            "npm cache clean --force",
            "pgrep -x uv",
            "uv cache clean",
            // wt remove looks for a git fsmonitor daemon.
            "pgrep -f git fsmonitor--daemon",
            "pgrep -f git fsmonitor--daemon",
        ]
        .map(String::from),
    );
    // One list in the survey, then a fresh list before each removal.
    expected.extend([list.clone(), list.clone(), list]);
    expected.extend([remove(&repo.pushed), remove(&repo.local)]);
    sb.assert_calls(expected);

    // The run.
    assert_eq!(run["mode"], "cleanup");
    assert_eq!(run["steps"], json!(["survey", "cleanup"]));
    assert!(run["finished_at"].is_string());
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
    // wt reports a merged branch with changes as "behind", not
    // "integrated". The reason must name the changes, not the state.
    assert_eq!(ask(&repo.dirty), "has changes: modified");
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
    assert_eq!(run["steps"], json!(["survey", "dry-run"]));
    assert_eq!(item_at(&run, &repo.pushed)["result"], "planned");
    assert_eq!(item_named(&run, "Homebrew cache")["result"], "planned");
    assert_eq!(sb.run_file(), run);
}

#[test]
fn survey_writes_the_run_file_and_report_prints_it() {
    let sb = Sandbox::new("survey");
    let repo = sb.make_repo();
    let before = sb.snapshot();

    let out = sb.maid(&["survey", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    assert_eq!(sb.snapshot(), before);
    let run = out.json();
    assert_eq!(run["mode"], "survey");
    assert_eq!(run["steps"], json!(["survey"]));
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

/// A branch at the same commit as main has wt state "empty": everything
/// in it is in main, so it passes the integrated check.
#[test]
fn worktree_at_main_is_cleaned() {
    let sb = Sandbox::new("empty");
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
    assert_eq!(item["decision"], "clean", "{item}");
    assert!(item["commands"][0].as_str().unwrap().contains("remove"));
}

#[test]
fn only_cleans_the_named_target() {
    let sb = Sandbox::new("only");
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
    expected.extend(["pgrep -x uv", "uv cache clean"].map(String::from));
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
    let brew: Vec<_> = calls.iter().filter(|c| c.starts_with("brew")).collect();
    assert_eq!(brew, ["brew --cache"], "{calls:?}");
    assert!(calls.contains(&"uv cache clean".to_string()), "{calls:?}");
}

/// --only must not hide the reason a target was already skipped.
#[test]
fn only_keeps_the_skip_reason() {
    let sb = Sandbox::new("only-running");
    sb.set("running", &["brew"]);

    let out = sb.maid(&["cleanup", "--only", "uv cache", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();
    assert_eq!(
        item_named(&run, "Homebrew cache")["reason"],
        "brew is running"
    );
    assert_eq!(
        item_named(&run, "npm cache")["reason"],
        "not selected by --only"
    );
}

#[test]
fn pgrep_failure_skips_the_target() {
    let sb = Sandbox::new("pgrep-broken");
    sb.set("pgrep-broken", &[]);

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

/// maid still tries a target whose program is missing. Its failure is
/// reported with a hint to run doctor, but does not fail the run.
#[test]
fn missing_program_failure_does_not_fail_the_run() {
    let sb = Sandbox::new("missing-program");
    // A fixed path, so maid gets to the clean command.
    sb.write_config(&DEFAULTS.replace(
        r#"locate          = "uv cache dir""#,
        r#"path = "~/.cache/uv""#,
    ));
    fs::remove_file(sb.root.join("bin/uv")).unwrap();

    let out = sb.maid(&["cleanup", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();
    let uv = item_named(&run, "uv cache");
    assert_eq!(uv["result"], "failed", "{uv}");
    assert_eq!(uv["missing_programs"], json!(["uv"]), "{uv}");
    assert!(uv["size_bytes"].as_u64().is_some(), "{uv}");
    let error = uv["error"].as_str().unwrap();
    assert!(
        error.ends_with("uv not on PATH: run `maid doctor` for details"),
        "{error}"
    );
    assert!(
        run["warnings"].as_array().unwrap().contains(&json!(
            "uv cache: uv not on PATH (run `maid doctor` for details)"
        )),
        "{run}"
    );
    assert!(!is_empty_dir(&sb.home.join(".cache/uv")));
    assert_eq!(item_named(&run, "npm cache")["result"], "done");
    assert_eq!(item_named(&run, "npm cache")["missing_programs"], json!([]));
}

/// A target whose locate command fails is skipped, and its clean command
/// does not run.
#[test]
fn failed_locate_skips_the_target() {
    let sb = Sandbox::new("missing-locate");
    fs::remove_file(sb.root.join("bin/uv")).unwrap();

    let out = sb.maid(&["cleanup", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();
    let uv = item_named(&run, "uv cache");
    assert_eq!(uv["result"], "skipped", "{uv}");
    assert_eq!(uv["path"], json!(null), "{uv}");
    assert_eq!(uv["reason"], "locate failed", "{uv}");
    let error = uv["error"].as_str().unwrap();
    assert!(
        error.starts_with(r#"locate "uv cache dir" failed: "#),
        "{error}"
    );
    assert!(!is_empty_dir(&sb.home.join(".cache/uv")));
    assert_eq!(item_named(&run, "npm cache")["result"], "done");
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
    sb.set("docker-running", &[]);

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
            "docker image prune --all --force --filter until=720h"
        ]
    );
}

#[test]
fn missing_wt_skips_the_repo() {
    let sb = Sandbox::new("no-wt");
    let repo = sb.make_repo();
    fs::remove_file(sb.root.join("bin/wt")).unwrap();

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
    sb.add_config(
        "[[target]]\nname = \"x\"\naction = \"keep\"\npath = \"~/x\"\nwhy = \"y\"\ncolour = \"red\"\n",
    );
    assert_fails(&sb.maid(&["survey"]), 2, "unknown field `colour`");
    assert!(sb.calls().is_empty());
}

#[test]
fn writable_config_is_refused() {
    let sb = Sandbox::new("writable");
    fs::set_permissions(sb.config_file(), fs::Permissions::from_mode(0o620)).unwrap();
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
        "run `maid init`",
    );
}

#[test]
fn only_worktree_removes_only_default_worktrees() {
    let sb = Sandbox::new("only-worktree");
    let repo = sb.make_repo();

    let out = sb.maid(&["cleanup", "--only", "worktree", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();

    assert!(!repo.pushed.exists());
    assert!(!repo.local.exists());
    assert!(repo.t3.exists());
    for cache in ["Library/Caches/Homebrew", ".cache/uv", ".npm/_cacache"] {
        assert!(!is_empty_dir(&sb.home.join(cache)), "{cache}");
    }
    assert_eq!(
        item_named(&run, "uv cache")["reason"],
        "not selected by --only"
    );
    assert_eq!(item_at(&run, &repo.pushed)["result"], "done");
}

#[test]
fn unknown_only_name_is_a_usage_error() {
    let sb = Sandbox::new("bad-only");
    assert_fails(
        &sb.maid(&["cleanup", "--only", "nope"]),
        2,
        "--only \"nope\" matches no target or worktree rule",
    );
    assert!(sb.calls().is_empty());
}

#[test]
fn report_errors() {
    let sb = Sandbox::new("report");
    for command in ["init", "doctor", "survey", "cleanup"] {
        assert_fails(
            &sb.maid(&[command, "--format", "html"]),
            2,
            "html output is available only for `maid report`",
        );
    }
    assert!(sb.calls().is_empty());
    assert_fails(&sb.maid(&["report"]), 1, "run `maid survey` first");
    assert_fails(
        &sb.maid(&["report", "--format", "html"]),
        1,
        "run `maid survey` first",
    );
}

#[test]
fn html_report_reads_the_saved_run_without_commands() {
    let sb = Sandbox::new("report-html");
    assert_eq!(sb.maid(&["survey"]).code, 0);
    let before = sb.calls();
    let files = sb.run_files();
    for args in [
        vec!["report", "--format", "html"],
        vec!["--format", "html", "report"],
    ] {
        let out = sb.maid(&args);
        assert_eq!(out.code, 0, "{out}");
        assert!(out.stdout.starts_with("<!doctype html>"), "{out}");
        assert!(out.stdout.contains("Homebrew cache"), "{out}");
        assert!(out.stdout.contains("maid survey"), "{out}");
    }
    assert_eq!(sb.calls(), before);
    assert_eq!(sb.run_files(), files);
}

#[test]
fn report_selects_runs_and_reuses_only_their_own_html() {
    let sb = Sandbox::new("report-runs");
    let survey = sb.maid(&["survey", "-f", "json"]).json();
    let cleanup = sb.maid(&["cleanup", "--dry-run", "-f", "json"]).json();
    let files = sb.run_files();
    let id = files[0].file_stem().unwrap().to_str().unwrap();
    let filename = files[0].file_name().unwrap().to_str().unwrap();
    let calls = sb.calls();

    for selected in [id, filename] {
        assert_eq!(
            sb.maid(&["report", "--run", selected, "-f", "json"]).json(),
            survey
        );
    }
    assert_eq!(sb.maid(&["report", "-f", "json"]).json(), cleanup);
    let old = sb.maid(&["report", "--run", id, "-f", "html"]);
    assert_eq!(old.code, 0, "{old}");
    let old_html = files[0].with_extension("html");
    assert_eq!(fs::read_to_string(&old_html).unwrap(), old.stdout);
    assert_eq!(
        fs::metadata(&old_html).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(old.stderr.contains("saved"), "{old}");
    fs::write(&old_html, "cached survey").unwrap();
    let reused = sb.maid(&["report", "--run", id, "-f", "html"]);
    assert_eq!(reused.stdout, "cached survey");
    assert!(reused.stderr.contains("using"), "{reused}");

    let latest = sb.maid(&["report", "-f", "html"]);
    assert_eq!(latest.code, 0, "{latest}");
    assert!(latest.stdout.contains("maid cleanup --dry-run"), "{latest}");
    assert_eq!(fs::read_to_string(&old_html).unwrap(), "cached survey");
    let forced = sb.maid(&["report", "--run", id, "-f", "html", "--force"]);
    assert_eq!(forced.code, 0, "{forced}");
    assert_eq!(forced.stdout, old.stdout);
    assert_eq!(
        fs::read_to_string(files[1].with_extension("html")).unwrap(),
        latest.stdout
    );
    assert_eq!(sb.calls(), calls);
    assert_eq!(sb.run_files(), files);
}

#[test]
fn report_rejects_missing_runs_and_force_without_html() {
    let sb = Sandbox::new("report-flags");
    assert_eq!(sb.maid(&["survey"]).code, 0);
    for id in ["missing", "../outside", "/tmp/outside.json"] {
        assert_fails(&sb.maid(&["report", "--run", id]), 1, "no run");
    }
    for format in ["text", "json"] {
        assert_fails(
            &sb.maid(&["report", "-f", format, "--force"]),
            2,
            "--force requires --format html",
        );
    }
    assert!(!sb.run_files()[0].with_extension("html").exists());
}

#[test]
fn added_target_is_cleaned() {
    let sb = Sandbox::new("added-target");
    let cache = sb.home.join("scratch");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("f"), "x").unwrap();
    sb.add_config(
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
fn init_writes_the_config_that_survey_needs() {
    let sb = Sandbox::new("init");
    fs::remove_file(sb.config_file()).unwrap();
    assert_fails(&sb.maid(&["survey"]), 2, "run `maid init`");
    assert!(sb.calls().is_empty());

    let out = sb.maid(&["init"]);
    assert_eq!(out.code, 0, "{out}");
    assert_eq!(fs::read_to_string(sb.config_file()).unwrap(), DEFAULTS);
    let mode = fs::metadata(sb.config_file()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    assert_fails(&sb.maid(&["init"]), 2, "exists already");
    assert_eq!(sb.maid(&["survey"]).code, 0);
}

/// Every fake is on PATH, so init above adds every known target. Here
/// bun is missing, so init leaves out the Bun target.
#[test]
fn init_leaves_out_targets_whose_program_is_missing() {
    let sb = Sandbox::new("init-detect");
    fs::remove_file(sb.config_file()).unwrap();
    fs::remove_file(sb.root.join("bin/bun")).unwrap();

    let out = sb.maid(&["init"]);
    assert_eq!(out.code, 0, "{out}");
    assert!(out.stdout.contains("Bun cache (needs bun)"), "{out}");
    let config = fs::read_to_string(sb.config_file()).unwrap();
    assert!(!config.contains("Bun cache"), "{config}");
    assert!(config.contains("Homebrew cache"), "{config}");
    assert!(config.contains("[worktrees]"), "{config}");

    let doctor = sb.maid(&["doctor"]);
    assert!(!doctor.stdout.contains("FAIL"), "{doctor}");
}

#[test]
fn each_run_has_its_own_file_and_report_prints_the_newest() {
    let sb = Sandbox::new("runs");
    let survey = sb.maid(&["survey", "--format", "json"]);
    let dry_run = sb.maid(&["cleanup", "--dry-run", "--format", "json"]);
    assert_eq!(dry_run.code, 0, "{dry_run}");
    assert_eq!(sb.run_files().len(), 2);
    assert_ne!(survey.json(), dry_run.json());

    let report = sb.maid(&["report", "--format", "json"]);
    assert_eq!(report.json(), dry_run.json());
}

/// maid writes the run file during a cleanup, so a stopped run shows how
/// far it got. The fake `uv cache clean` copies it.
#[test]
fn run_file_shows_the_run_so_far() {
    let sb = Sandbox::new("so-far");
    let out = sb.maid(&["cleanup", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");

    let text = fs::read_to_string(sb.root.join("mid-run.json")).unwrap();
    let mid: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(mid["mode"], "cleanup");
    assert_eq!(mid["steps"], json!(["survey"]));
    assert_eq!(mid["finished_at"], json!(null));
    assert_eq!(item_named(&mid, "Homebrew cache")["result"], "done");
    assert_eq!(item_named(&mid, "uv cache")["result"], "planned");
    assert_eq!(item_named(&out.json(), "uv cache")["result"], "done");

    // A stopped run: report and doctor say it did not finish.
    fs::write(sb.run_files().last().unwrap(), text).unwrap();
    let report = sb.maid(&["report"]);
    assert!(
        report.stdout.contains("not finished (ran: survey)"),
        "{report}"
    );
    let doctor = sb.maid(&["doctor"]);
    assert!(doctor.stdout.contains("did not finish"), "{doctor}");
}

#[test]
fn doctor_checks_the_config() {
    let sb = Sandbox::new("doctor");
    let out = sb.maid(&["doctor"]);
    assert_eq!(out.code, 0, "{out}");
    assert!(!out.stdout.contains("FAIL"), "{out}");
    assert!(out.stdout.contains("ok    config"), "{out}");
    assert!(out.stdout.contains("ok    last run  none yet"), "{out}");

    assert!(
        out.stdout.contains("ok    target    Homebrew cache"),
        "{out}"
    );
    assert!(!out.stdout.contains("warn  target"), "{out}");

    // A target without needs is a warning, not a failure.
    sb.add_config(
        "[[target]]\nname = \"Scratch\"\naction = \"keep\"\npath = \"~/scratch\"\nwhy = \"mine\"\n",
    );
    let out = sb.maid(&["doctor"]);
    assert_eq!(out.code, 0, "{out}");
    assert!(
        out.stdout.contains("warn  target    Scratch: no `needs`"),
        "{out}"
    );

    // A target whose program is missing fails.
    fs::remove_file(sb.root.join("bin/bun")).unwrap();
    let out = sb.maid(&["doctor"]);
    assert_eq!(out.code, 1, "{out}");
    assert!(
        out.stdout
            .contains("FAIL  target    Bun cache: bun not on PATH"),
        "{out}"
    );

    fs::remove_file(sb.config_file()).unwrap();
    let out = sb.maid(&["doctor"]);
    assert_eq!(out.code, 1, "{out}");
    assert!(out.stdout.contains("FAIL  config"), "{out}");
    assert!(out.stdout.contains("run `maid init`"), "{out}");
}

#[test]
fn doctor_json_has_the_checks_and_the_last_run() {
    let sb = Sandbox::new("doctor-json");
    let out = sb.maid(&["doctor", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let health = out.json();
    assert_eq!(health["ok"], true);
    assert_eq!(health["last_run"], json!(null));
    let checks = health["checks"].as_array().unwrap();
    assert!(checks.contains(&json!({
        "level": "ok", "check": "target", "detail": "Homebrew cache"
    })));

    let cleanup = sb.maid(&["cleanup", "--only", "uv cache"]);
    assert_eq!(cleanup.code, 0, "{cleanup}");
    let health = sb.maid(&["doctor", "-f", "json"]).json();
    let last = &health["last_run"];
    assert_eq!(last["mode"], "cleanup");
    assert_eq!(last["steps"], json!(["survey", "cleanup"]));
    assert_eq!(last["done"], 1);
    assert_eq!(last["failed"], 0);
    assert!(last["freed_bytes"].as_u64().unwrap() > 0, "{last}");
    assert!(last["file"].as_str().unwrap().ends_with(".json"));
}

/// A deleted fake is missing: PATH has no system folder, so the real tool
/// (for example /usr/bin/docker) cannot run in its place.
#[test]
fn deleted_fake_does_not_fall_back_to_the_real_tool() {
    let sb = Sandbox::new("no-real-docker");
    fs::remove_file(sb.root.join("bin/docker")).unwrap();

    let found = sb
        .command("sh")
        .args(["-c", "command -v docker"])
        .output()
        .unwrap();
    assert!(!found.status.success(), "{found:?}");

    // A dry run, so that even a broken PATH cannot prune real images.
    let out = sb.maid(&["cleanup", "--dry-run", "--format", "json"]);
    assert_eq!(out.code, 0, "{out}");
    let run = out.json();
    for name in ["Docker images", "Docker build cache"] {
        let item = item_named(&run, name);
        assert_eq!(item["result"], "skipped", "{item}");
        assert_eq!(item["reason"], "only_if failed: docker info", "{item}");
        assert_eq!(item["missing_programs"], json!(["docker"]), "{item}");
    }
}

#[test]
#[should_panic(expected = "refusing to run maid")]
fn harness_refuses_a_home_outside_the_temp_folder() {
    let mut sb = Sandbox::new("guard");
    sb.home = "/".into();
    // report only reads the run file, in case the guard ever fails.
    sb.maid(&["report"]);
}
