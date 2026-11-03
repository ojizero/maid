<p align="center">
  <img src="assets/icon.png" alt="maid icon: a chibi maid with a broom" width="200">
</p>

# maid

`maid` frees disk space on a computer. It measures known places on disk ("targets") and git worktrees, and gives each one a decision: `clean`, `keep` or `ask`. It cleans an item only through the command of the tool that owns it (`brew cleanup`, `uv cache clean`, `wt remove`, ...). Then it reports what it did.

`maid` does not delete files itself. The only `rm -rf` is in the default config, for Xcode DerivedData, which has no owner command.

## Usage

```
maid [-c|--config PATH] init
maid [-c|--config PATH] [-f|--format text|json] doctor
maid [-c|--config PATH] [-f|--format text|json|html] survey
maid [-c|--config PATH] [-f|--format text|json|html] cleanup [-n|--dry-run] [--only NAME]...
maid [-f|--format text|json|html] report
```

`-c` and `-f` can also come after the command. The `html` format is not implemented yet: it fails with exit code 2 before maid does any work.

- `init` writes a config file with the known targets that this computer can use: each target in [`config/default.toml`](config/default.toml) whose `needs` programs are all on PATH. It lists the targets it left out. It does not replace a file that exists.
- `doctor` checks that maid can run, and that every target in the config is usable. It prints one line for each check: `ok`, `warn` or `FAIL`. It exits with code 1 when a check fails. See [Doctor](#doctor).
- `survey` measures every target and worktree, finds large folders that nothing covers, and decides what to do with each. It changes nothing.
- `cleanup` runs a fresh survey, then runs the clean command of each item with decision `clean`, one at a time. Just before each item it checks `skip_if_running`, `only_if` and the worktree checks again. When an item fails, maid records the error and continues. A target whose path is a symlink is skipped. If `pgrep` cannot check a program, the target is skipped.
  - `-n`, `--dry-run`: print each planned command with the current size, and run the item's `dry_run` command if it has one. Changes nothing. A `dry_run` command that fails marks the item `failed` (exit code 1).
  - `--only NAME`: clean only the target or worktree rule with this name. `worktree` selects the worktrees that no override matches. You can give it more than once.
- `report` prints the newest run file.

Results go to stdout. Progress and warnings go to stderr. The text output has no color when stdout is not a terminal or `NO_COLOR` is set.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success, including runs with `ask` items |
| 1 | Runtime failure, including a cleanup item that failed or a failed `doctor` check. An item whose `needs` program is missing does not count. |
| 2 | Usage or config error |

## Config

`maid init` writes the config file from [`config/default.toml`](config/default.toml), the known targets. The comments in that file describe every field. A target's `needs` lists the programs it uses, or is `[]` when it uses none. `init` adds a known target only when all of them are on PATH. `needs` is optional in your config, but `doctor` warns about a target without it, because it cannot check that target. It fails when a program in `needs` is missing. `survey` and `cleanup` still measure and clean such a target, and the run gets a warning that tells you to run `maid doctor`. If its command then fails, the item is `failed` with a hint to run `maid doctor`, but the run does not exit with 1 for it. `survey` and `cleanup` need the config file. If it is missing, they stop with exit code 2 and tell you to run `maid init`.

The config file is:

1. the file given with `-c PATH`;
2. else `$XDG_CONFIG_HOME/maid/config.toml`;
3. else `~/.config/maid/config.toml`.

The file is the whole config. maid does not add its defaults to it, so a target that you remove is not measured. The file must be owned by you and not writable by group or others, because it holds shell commands. `init` writes it with mode 0600.

`[[worktrees.override]]` entries are checked in order. The first match wins.

Example:

```toml
[[target]]
name   = "uv cache"
action = "keep"
path   = "~/.cache/uv"
why    = "I work offline this week"
```

This changes the `uv cache` target in the file that `init` wrote.

## Worktrees

maid finds main checkouts (folders with a `.git` folder, up to 4 levels deep) in each `[worktrees].repos` folder. For each one it runs `wt -C <repo> list --format=json`. If `wt` is missing or fails, the repo is reported as a skipped `ask` item.

A worktree with action `clean` is removed only when all of these are true. These checks are in the code, not in the config:

1. `wt` says its state is `integrated`, or `empty` (the same commit as the default branch).
2. It has no staged, modified, untracked, renamed, deleted or conflicted files.
3. Every ignored file (`git status --porcelain --ignored`) is inside a build folder (`node_modules`, `target`, `dist`, `build`, `_build`, `deps`, `.venv`, `venv`, `__pycache__`, `.pytest_cache`, `.next`, `.turbo`, `.parcel-cache`, `.gradle`, `coverage`, `.elixir_ls`) or is a `.DS_Store` file. A `.env`, a database or a `tmp/` folder fails this check.

If a check fails, the decision is `ask` and the reason names the check.

A survey does not change files, with one exception that comes from `wt`: its first run in a repo writes the default branch to `.git/config`, and it keeps a cache in `.git/wt`. maid runs git with `GIT_OPTIONAL_LOCKS=0`, so `git status` does not refresh the index.

## Unknown large folders

`survey` lists folders of 1 GiB or more that no target and no worktree covers. It scans the top-level folders of `$HOME` and the children of `~/Library/Caches`, `~/Library/Developer`, `~/.cache` and `~/.local/share`. The size of a folder does not include the targets and worktrees in it. maid measures with `du -skx`, so it does not read other volumes mounted inside a folder.

It does not scan the `$HOME` folders that macOS guards with a permission prompt (`Desktop`, `Documents`, `Downloads`, `Movies`, `Music`, `Pictures`, `.Trash`) or `Library`, `.cache` and `.local` as a whole.

## Doctor

`maid doctor` checks:

| Check | Fails when | Warns when |
|---|---|---|
| `config` | the config file is missing or not valid | |
| `state` | maid cannot write to the run file folder | |
| `last run` | | the newest run did not finish, or an item failed |
| `sh`, `du`, `pgrep` | the program is not on PATH | |
| `target` (one for each target) | a program in its `needs` is not on PATH | it has no `needs` |
| `git`, `wt` | | the program is not on PATH (only when `repos` is not empty) |
| `repos` (one for each folder) | | the folder does not exist |

The `last run` line sums up the newest run: items done, space freed, items failed and items to ask about. `-f json` prints the same as JSON:

```json
{
  "ok": true,
  "checks": [
    { "level": "ok | warn | fail", "check": "target", "detail": "Homebrew cache" }
  ],
  "last_run": {
    "file": "/Users/me/.local/state/maid/runs/2026-11-03T090040.123456789Z.json",
    "mode": "cleanup",
    "steps": ["survey", "cleanup"],
    "started_at": "2026-11-03T09:00:00Z",
    "finished_at": "2026-11-03T09:00:40Z",
    "done": 3,
    "failed": 0,
    "ask": 2,
    "freed_bytes": 8700000000
  }
}
```

`last_run` is `null` when there is no run yet.

## Run files

Each run writes a new file to `$XDG_STATE_HOME/maid/runs/`, else `~/.local/state/maid/runs/`. The file name is the UTC time to the nanosecond, for example `2026-11-03T090040.123456789Z.json`. maid writes a temp file and renames it, with mode 0600. maid keeps the newest 7 run files and deletes the older ones.

maid writes the run file while the run goes on: after the survey, then again after each item that `cleanup` cleans or dry-runs. So a run that was stopped still shows how far it got. `report` and `doctor` say when the newest run did not finish.

```json
{
  "version": 2,
  "host": "my-mac",
  "mode": "survey | dry-run | cleanup",
  "steps": ["survey", "cleanup"],
  "started_at": "2026-11-03T09:00:00Z",
  "finished_at": "2026-11-03T09:00:40Z",
  "free_bytes_before": 150000000000,
  "free_bytes_after": 150000000000,
  "items": [
    {
      "kind": "target | worktree | unknown",
      "name": "uv cache",
      "path": "/Users/me/.cache/uv",
      "repo": null,
      "branch": null,
      "size_bytes": 8700000000,
      "decision": "clean | keep | ask",
      "reason": null,
      "commands": ["uv cache clean"],
      "result": "planned | done | skipped | failed",
      "freed_bytes": null,
      "error": null,
      "missing_programs": []
    }
  ],
  "warnings": []
}
```

- `mode` is the command. `steps` lists the steps that finished, in order: `survey`, then `dry-run` or `cleanup`. A `cleanup` run with `"steps": ["survey"]` was stopped during the cleanup, or is still running.
- `finished_at` and `free_bytes_after` are `null` until the run finishes.
- `repo` and `branch` are set for worktrees.
- `missing_programs` lists the programs in the target's `needs` that were not on PATH. A failed item with missing programs does not set exit code 1.
- `commands` holds the planned commands, and after a cleanup the commands that ran.
- `freed_bytes` is the item's size before the clean minus its size after. It is not the change in free space on the disk.
- Free space is measured with `statvfs` on `/System/Volumes/Data` on macOS (`/` is the sealed system volume there), else on `/`.

## Development

Mise installs the toolchain and runs all tasks. Each task is a script in [`mise/tasks`](mise/tasks):

```
mise install
mise run dev -- survey     # build and run the dev binary
mise run test              # unit and end-to-end tests
mise run lint              # cargo fmt --check and clippy
mise run ci                # lint and test
mise run fmt               # format the code
mise run build             # release binary: target/release/maid
mise run dist              # release archives for macOS and Linux in dist/
mise run release           # ci and dist, then publish dist/ as a Forgejo release
```

`mise tasks` lists them.

### CI and releases

Forgejo Actions runs the same tasks. The workflows only install mise and call one task:

- [`ci.yml`](.forgejo/workflows/ci.yml) runs `mise run ci` on each push to `main` and on each pull request.
- [`release.yml`](.forgejo/workflows/release.yml) runs `mise run release` when you push a `v*` tag. It runs on the `sagittarius-a` runner, because zig fails at random on the Linux 4.4 kernel of the `docker` runner.

The [`mise` action](.forgejo/actions/mise/action.yml) installs a pinned mise and the tools. It caches mise, the tools, the Cargo registry and `target/` for each job.

To release:

1. Set `version` in `Cargo.toml`, then commit.
2. Tag that commit `v<version>` and push the tag.

`release` stops if HEAD does not have that tag. To run it locally, set `FORGEJO_TOKEN` to a token that can write to the repository.

`dist` cross-builds with `cargo zigbuild` for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-musl` and `x86_64-unknown-linux-musl`. mise can install a release. The forge requires sign-in, so set `MISE_FORGEJO_ENTERPRISE_TOKEN` to a token that can read the repository, then run:

```
mise use 'forgejo:ojizero/maid[api_url=https://forge.ojizero.dev/api/v1]'
```

Each command is a module with a `run` function: `init.rs`, `doctor.rs`, `survey.rs`, `cleanup.rs` and `report.rs`. A command with flags defines its own clap `Args` there. `app.rs` holds what all commands share: the home folder, the output format, the config, and saving and printing the run files (`record.rs`). To add a command, add its module and one arm in `main.rs`.

The end-to-end tests run the real binary in a sandbox folder, with fake tools first on `PATH` and real `git` and `wt`. They set `MAID_TEST_VOLUME` to measure free space on the sandbox folder. Only tests use this variable.

macOS is the main platform. The code also builds on Linux, but the defaults are macOS paths.
