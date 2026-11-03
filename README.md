<p align="center">
  <img src="assets/icon.png" alt="maid icon: a chibi maid with a broom" width="200">
</p>

# maid

`maid` frees disk space on a computer. It measures known places on disk ("targets") and git worktrees, and gives each one a decision: `clean`, `keep` or `ask`. It cleans an item only through the command of the tool that owns it (`brew cleanup`, `uv cache clean`, `wt remove`, ...). Then it reports what it did.

`maid` does not delete files itself. The only `rm -rf` is in the default config, for Xcode DerivedData, which has no owner command.

## Usage

```
maid [-c|--config PATH] [-f|--format text|json|html] survey
maid [-c|--config PATH] [-f|--format text|json|html] cleanup [-n|--dry-run] [--only NAME]...
maid [-f|--format text|json|html] report
```

`-c` and `-f` can also come after the command. The `html` format is not implemented yet: it fails with exit code 2 before maid does any work.

- `survey` measures every target and worktree, finds large folders that nothing covers, and decides what to do with each. It changes nothing.
- `cleanup` runs a fresh survey, then runs the clean command of each item with decision `clean`, one at a time. Just before each item it checks `skip_if_running`, `only_if` and the worktree checks again. When an item fails, maid records the error and continues. A target whose path is a symlink is skipped. If `pgrep` cannot check a program, the target is skipped.
  - `-n`, `--dry-run`: print each planned command with the current size, and run the item's `dry_run` command if it has one. Changes nothing. A `dry_run` command that fails marks the item `failed` (exit code 1).
  - `--only NAME`: clean only the target or worktree rule with this name. `worktree` selects the worktrees that no override matches. You can give it more than once.
- `report` prints the last run file.

Results go to stdout. Progress and warnings go to stderr. The text output has no color when stdout is not a terminal or `NO_COLOR` is set.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success, including runs with `ask` items |
| 1 | Runtime failure, including a cleanup item that failed |
| 2 | Usage or config error |

## Config

maid embeds its defaults: [`config/default.toml`](config/default.toml). The comments in that file describe every field.

The user file adds to the defaults. maid reads the first of:

1. the file given with `-c PATH` (it must exist);
2. `$XDG_CONFIG_HOME/maid/config.toml`;
3. `~/.config/maid/config.toml`.

If there is no user file, only the defaults apply. The user file must be owned by you and not writable by group or others, because it holds shell commands.

- A `[[target]]` with the same `name` as a default replaces it. A new name adds a target.
- Keys in `[worktrees]` replace the default keys.
- User `[[worktrees.override]]` entries are checked before the default ones. The first match wins.

Example:

```toml
[[target]]
name   = "uv cache"
action = "keep"
path   = "~/.cache/uv"
why    = "I work offline this week"

[worktrees]
delete_branch = "never"
```

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

## Run file

Each run writes `$XDG_STATE_HOME/maid/run.json`, else `~/.local/state/maid/run.json`. maid writes a temp file and renames it, with mode 0600.

```json
{
  "version": 1,
  "host": "my-mac",
  "mode": "survey | dry-run | cleanup",
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
      "error": null
    }
  ],
  "warnings": []
}
```

- `repo` and `branch` are set for worktrees.
- `commands` holds the planned commands, and after a cleanup the commands that ran.
- `freed_bytes` is the item's size before the clean minus its size after. It is not the change in free space on the disk.
- Free space is measured with `statvfs` on `/System/Volumes/Data` on macOS (`/` is the sealed system volume there), else on `/`.

## Development

Mise installs the toolchain and runs all tasks:

```
mise install
mise run dev -- survey     # build and run the dev binary
mise run test              # unit and end-to-end tests
mise run lint              # cargo fmt --check and clippy
mise run check             # lint and test
mise run fmt               # format the code
mise run build             # release binary: target/release/maid
```

`mise tasks` lists them.

Each command is a module with a `run` function: `survey.rs`, `cleanup.rs` and `report.rs`. A command with flags defines its own clap `Args` there. `app.rs` holds what all commands share: the home folder, the output format, the config, and saving and printing the run file (`record.rs`). To add a command, add its module and one arm in `main.rs`.

The end-to-end tests run the real binary in a sandbox folder, with fake tools first on `PATH` and real `git` and `wt`. They set `MAID_TEST_VOLUME` to measure free space on the sandbox folder. Only tests use this variable.

macOS is the main platform. The code also builds on Linux, but the defaults are macOS paths.
