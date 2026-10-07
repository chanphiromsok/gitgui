# gitgui

A Git GUI in Rust, on GPUI. Feature inventory and architecture: `docs/feature-spec.md`.

## Build and run

You need [Rust](https://rustup.rs) (stable, 1.90 or newer) and `git` on your PATH. The app runs your own `git`, so your config, hooks and credentials apply.

```sh
cargo run --release -p gitgui-app -- /path/to/repo   # open the window (the path is optional)
cargo test --workspace                               # all tests
```

Always use `--release` to run it. A debug build of the UI is far too slow to scroll.

Saved projects, settings and comments live in `~/Library/Application Support/gitgui` (macOS), `%APPDATA%\gitgui` (Windows) or `~/.local/share/gitgui` (Linux). Set `GITGUI_DATA_DIR` to use another folder.

## Release build: macOS

Needs Xcode or the Command Line Tools (`xcode-select --install`). The Metal shader compiler is **not** needed: the app compiles its shaders when it starts (`runtime_shaders`).

```sh
scripts/bundle-macos.sh             # this Mac's architecture
scripts/bundle-macos.sh --universal # Apple Silicon + Intel in one app
```

This writes `dist/gitgui.app` and `dist/gitgui-<version>.dmg`. Drag the app to Applications.

**Signing.** The script signs ad hoc, which is enough to run on the Mac that built it. On another Mac, Gatekeeper blocks it: right-click the app and choose Open once, or run `xattr -dr com.apple.quarantine gitgui.app`.
To distribute properly you need an Apple Developer ID: sign with `codesign --force --deep --options runtime --sign "Developer ID Application: NAME (TEAMID)" dist/gitgui.app`, then notarize with `xcrun notarytool submit dist/gitgui-<version>.dmg --keychain-profile PROFILE --wait` and `xcrun stapler staple dist/gitgui-<version>.dmg`.

## Release build: Windows

Build it on Windows (cross-compiling GPUI from another OS is not supported). **This has not been built or run on Windows yet.** The code compiles for it in principle (GPUI supports Windows, and the data folder uses `%APPDATA%`), but see the gaps below.

1. Install [Rust](https://rustup.rs) with the default `x86_64-pc-windows-msvc` toolchain.
2. Install **Visual Studio Build Tools** with the *Desktop development with C++* workload and a Windows 10/11 SDK.
3. Install [Git for Windows](https://git-scm.com/download/win) and make sure `git` works in a terminal.
4. In PowerShell, from the project folder:

```powershell
cargo build --release -p gitgui-app
```

The program is `target\release\gitgui-app.exe`. It is a single file: copy it anywhere. For an installer, wrap it with [WiX](https://wixtoolset.org) or [Inno Setup](https://jrsoftware.org/isinfo.php); sign it with `signtool` to avoid SmartScreen warnings.

Known Windows gaps, to fix before a public release:
- Shortcuts are written for the Mac (`cmd-o`, `cmd-,`, `cmd-e`, and the text field's `cmd-c/v/x/a`). On Windows they should be `ctrl-…`; the menu bar is also macOS-only.
- The code font is `Menlo`, which Windows does not have; set `MONO` in `crates/app/src/ui.rs` to `Consolas`.
- Git is found on PATH only.

## Release build: Linux

Install the system libraries GPUI needs (on Debian/Ubuntu: `libxkbcommon-x11-dev libwayland-dev libxcb1-dev libfontconfig-dev libssl-dev pkg-config`), then `cargo build --release -p gitgui-app`. Not tested either, and it has the same shortcut and font gaps as Windows.

## Layout

| Crate | What it is | Dependencies |
|---|---|---|
| `crates/core` | Repo model, `Backend` trait, `GitCli` backend, lane layout, diff parser, file tree | none (std only) |
| `crates/store` | Saved projects and line comments, as JSON in the app-data folder | serde |
| `crates/cli` | `gitgui log [PATH] [-n N]` prints the commit graph as text | core |
| `crates/app` | The GPUI window | core, store, gpui 0.2.2 |

`crates/app` is not in `default-members`, so a plain `cargo test` stays fast. Use `cargo test --workspace` for everything
and `cargo run -p gitgui-app -- [PATH]` for the window.

## What the window does

Left to right: **projects** | **graph** | **file pane**. Both dividers drag to resize.

- **Projects:** *Open Folder…* (Cmd-O) adds a repository (a folder inside one adds the repo). Kept between launches; hover a row and click × to remove.
- **Graph:** Sourcetree-style table.
  - Each branch is one line with one color from its tip down to the commit it was branched from. Lines are not bent into their parent early, so the fork point is visible.
  - A fork commit gets a ring and a *branch point · feat/x* chip. Selecting a commit brings its branch line forward and dims the rest.
  - Icons tell a pull request, a merge and a plain commit apart. A local branch and its remote share one badge; the current branch is ringed and bold.
  - Stashes are one `stash@{n}` commit; an *Uncommitted Changes* row sits on top.
- **Grouping** (Settings… / Cmd-,, on by default): a pull request's commits are listed one level in under it, with a guide line, and fold away with the arrow.
  Squash-merged branches go under their squash commit.
- **Already merged?** A background scan marks branches that are merged even when git cannot tell (squash and rebase merges): *squash-merged into release/1.0.0 · #37*, and *squash of feat/x* on the commit that carries it.
  Evidence, strongest first: identical changes in one trunk commit; the same pull request number; a trunk commit repeating the branch's commit messages. The last two are shown in italics as guesses.
- **Right-click** a branch badge or a commit: checkout, rename, delete, merge into current, rebase current onto, push, new branch / tag here, cherry-pick, copy name / SHA / message.
  Anything that rewrites history, deletes, or reaches a remote asks first. Nothing is forced: no force-push, no `reset --hard`, and an unmerged branch is deleted only after a second, explicit yes.
  A merge, rebase or cherry-pick that meets conflicts stops and the banner offers **Abort**, which puts everything back.
- **File pane** (click a commit): overview, and the changed files as a **tree or flat list** with a filter. A file opens as a **unified or split** diff. **Expand** (Cmd-E) gives it the whole area.
- **Comments:** hover a diff line and click **+**. Saved locally per repository by commit, file, side and line, so they never go stale.
- **Shortcuts:** Cmd-O open, Cmd-, settings, Cmd-R refresh, Cmd-E full view, Esc back (closes a menu, dialog or settings first), Cmd-W close window, Cmd-M minimize, Cmd-Q quit.

`gitgui merges PATH` prints the merge scan for a repository from the terminal (read-only).

## Tests

`cargo test --workspace`: 155 tests. Core and store run against real temporary git repositories, including real squash merges, conflicts, a local "remote" for push, and a 300-case random test that grouping never puts a commit above its parent.
The app tests run the real window headlessly (GPUI's test platform) through every flow and draw a frame after each step, so a view that panics on real data fails.
Frame-time checks keep a 10,000-row diff and a 4,400-row graph under 8 ms per frame in release builds.
They do not check how anything looks.

## Known gaps

- Comments are one line each and local only. Syncing with GitHub/GitLab review comments is not built.
- No word-level highlight inside changed lines; long lines are clipped, not scrolled.
- Divider positions and folded groups are not remembered between launches.
- The merge scan looks at each trunk's newest 500 commits and gives up after 25 s (it says how many branches it skipped).
- Menu items from Sourcetree not built: *Create Archive*, *Unselect in Branches Dropdown* (there is no branch dropdown yet).
- `crates/app/src/text_input.rs` is adapted from gpui's `input` example (Apache-2.0, Zed Industries).
- 