# Rust Git GUI — Feature Inventory & Architecture Sketch

Compiled from: Fork, GitKraken, Sublime Merge, Tower, GitUp, Sourcetree, VS Code Git Graph, GitLens,
JetBrains Git, lazygit, tig, Jujutsu (jj).
Built from prior knowledge of these tools (not a live scrape) — verify details against each product before copying behavior.

**Source key:** F=Fork · GK=GitKraken · SM=Sublime Merge · T=Tower · GU=GitUp · ST=Sourcetree ·
GG=Git Graph (VS Code) · GL=GitLens · JB=JetBrains · LG=lazygit · TG=tig · JJ=Jujutsu

**Priority:** P0 = MVP (usable daily) · P1 = v1 (competitive) · P2 = differentiators / later

---

## 1. Repository management

| Feature | Source | Pri |
|---|---|---|
| Open / init / clone (HTTPS + SSH) | all | P0 |
| Recent repos list | all | P0 |
| Multiple repos as tabs | F, GK, SM, T | P0 |
| Repo manager with folders/groups, bookmarks | F, T, ST | P1 |
| Workspaces (group repos, run actions across them) | GK, GL | P2 |
| Remotes: add / edit / remove / rename | all | P0 |
| Submodules: init / update / status / open | F, T, GK | P1 |
| Worktrees: list / add / remove / open | T, GL, LG | P1 |
| Bare repo + sparse checkout + partial clone support | — | P2 |
| Git LFS awareness (pointer files, lock, pull) | F, T, GK | P1 |
| Open in terminal / editor / file manager | all | P0 |
| Credential handling (keychain, SSH agent, credential helpers) | all | P0 |
| OAuth sign-in to GitHub / GitLab / Bitbucket / Azure DevOps | F, GK, T | P1 |

## 2. Commit graph & history  *(the core differentiator)*

| Feature | Source | Pri |
|---|---|---|
| Lane-based commit graph with colored branches | all | P0 |
| Branch / tag / HEAD / remote-ref labels on commits | all | P0 |
| Virtualized rendering (100k–1M+ commits stay smooth) | GU, SM | P0 |
| Incremental / lazy history loading | GG, SM | P0 |
| Commit detail panel: message, author, committer, parents, refs, stats | all | P0 |
| Changed-files list per commit (tree and flat view) | F, SM, GK | P0 |
| Per-commit diff viewer | all | P0 |
| Filter by branch(es), author, date range, path | GG, GK, JB, SM | P1 |
| Show/hide branches, "only this branch", first-parent-only view | GG, GK | P1 |
| Search: message, author, hash, file, content (pickaxe), regex | SM, GK, JB | P1 |
| Search query syntax (`author:`, `file:`, `message:`, …) | SM | P2 |
| Compare any two commits / branches / tags | GG, GK, T | P1 |
| File history & folder history | F, GL, JB | P1 |
| Blame / annotate (gutter + inline) | F, SM, GL, JB | P1 |
| Reflog browser | GU, T, LG | P1 |
| GPG/SSH signature verification badge | GK, SM | P1 |
| Avatars (Gravatar / provider) | GG, GK, F | P1 |
| Jump to HEAD / ref / hash, keyboard navigation | LG, TG, GG | P0 |
| Commit graph minimap / density overview | — | P2 |
| Code-review mode (mark files as reviewed per range) | GG | P2 |
| Live graph — updates instantly on repo change | GU | P1 |

## 3. Working copy & staging

| Feature | Source | Pri |
|---|---|---|
| Status: unstaged / staged / untracked / conflicted | all | P0 |
| Stage / unstage file | all | P0 |
| Stage / unstage hunk | all | P0 |
| Stage / unstage individual lines | SM, F, LG, JB | P0 |
| Discard changes (file / hunk / line) with confirmation | all | P0 |
| Diff: unified + side-by-side | all | P0 |
| Intra-line (word-level) diff highlighting | SM, F, JB | P1 |
| Syntax-highlighted diffs | SM, F, GK | P1 |
| Ignore-whitespace, context-lines, EOL options | F, SM, GG | P1 |
| Image diff (side-by-side / onion / swipe) | F, GK, T | P2 |
| Large-file / binary-file handling | all | P0 |
| Commit, amend, sign-off, GPG/SSH sign | all | P0 |
| Commit message templates & conventional-commit helper | GK, GL, JB | P1 |
| Commit message: subject length hint, spellcheck | SM, GK | P1 |
| Run pre-commit / commit-msg hooks with output panel | LG, T | P1 |
| Stash: create (incl. partial) / apply / pop / drop / view diff | all | P0 |
| `.gitignore` editor / "ignore this file" action | F, ST, T | P1 |
| Patch create / apply | F, LG, T | P2 |
| Custom patch builder (move lines between commits) | LG | P2 |
| Shelve / changelists | JB | P2 |

## 4. Branching & history rewriting

| Feature | Source | Pri |
|---|---|---|
| Create / rename / delete branch (local + remote) | all | P0 |
| Checkout branch, tag, commit (detached) | all | P0 |
| Checkout remote branch → auto-create tracking branch | all | P0 |
| Ahead/behind indicators vs. upstream | all | P0 |
| Merge: ff / no-ff / squash, merge preview | all | P0 |
| Rebase onto branch | all | P1 |
| **Interactive rebase editor** (pick/reword/edit/squash/fixup/drop, drag-to-reorder) | F, GL, LG, GU | P1 |
| Cherry-pick (single / range / copy-paste flow) | all | P1 |
| Revert commit | all | P1 |
| Reset: soft / mixed / hard to a commit | all | P1 |
| Split a commit into multiple | GU, LG | P2 |
| Move / reorder / squash commits via drag-and-drop | GU, GK, T | P2 |
| Fixup / autosquash workflows | LG, F | P2 |
| Tags: lightweight + annotated, push/delete | all | P0 |
| Bisect (guided good/bad UI) | LG, T | P2 |
| Git-flow / branch-naming presets | GK, F, ST, T | P2 |
| Drag-and-drop branch ops (drag A onto B → merge/rebase menu) | GK, T | P2 |
| Branch comparison / "what's different" view | T, GG | P1 |

## 5. Conflict resolution

| Feature | Source | Pri |
|---|---|---|
| Conflicted-files list with state | all | P0 |
| Built-in **3-way merge editor** (ours / theirs / result) | F, SM, GL, JB, GK | P1 |
| One-click accept ours / theirs / both | all | P0 |
| Launch external merge tool | all | P1 |
| Conflict-state banner during merge/rebase/cherry-pick + continue/abort | all | P0 |
| `rerere` support | — | P2 |
| First-class conflicts (conflicts committed, resolved later) | JJ | P2 |

## 6. Remotes & collaboration

| Feature | Source | Pri |
|---|---|---|
| Fetch / pull (merge or rebase) / push | all | P0 |
| Force-push with `--force-with-lease` guard | all | P0 |
| Background auto-fetch | all | P1 |
| Prune stale remote branches | all | P1 |
| Push tags, set upstream | all | P0 |
| Pull-request list / view / create / checkout locally | GK, T, GL, JB | P1 |
| CI status / checks on commits and PRs | GK, GL | P2 |
| Issue linking in commit messages / hover cards | GK, GL, ST | P2 |
| "Launchpad": unified list of my PRs / reviews / issues | GK, GL | P2 |

## 7. Safety, undo & recovery

| Feature | Source | Pri |
|---|---|---|
| **Global undo / redo of repo operations** | GU, GK, T, JJ | P1 |
| Auto-snapshots before destructive ops (backed by reflog / stash) | GU | P1 |
| Operation log (every action recorded, revertible) | JJ, LG | P2 |
| Confirm dialogs for destructive actions with preview of what's lost | all | P0 |
| Command log: show the exact `git` commands executed | LG, ST | P1 |
| Recover dropped stash / deleted branch from reflog | GU, T | P1 |

## 8. UX, navigation & customization

| Feature | Source | Pri |
|---|---|---|
| Command palette / quick launcher | F, SM, LG | P1 |
| Fully keyboard-driven workflow, vim-style optional | LG, TG | P1 |
| Context menus on every ref/commit/file | all | P0 |
| Light / dark themes, system follow | all | P0 |
| Custom themes, font & density settings | SM, GK | P2 |
| Resizable / dockable panels, saved layouts | GK, SM | P1 |
| Custom commands / actions (user-defined shell snippets) | F, LG | P2 |
| Integrated terminal | GK, LG, JB | P2 |
| Notifications & progress for long operations, cancelable | all | P0 |
| i18n | F, GK | P2 |
| Drag-and-drop file paths / repo folders onto window | all | P1 |

## 9. Cross-cutting / non-functional

- **Speed:** cold-open a 500k-commit repo in < 1 s to first paint (GitUp/Sublime Merge benchmark). Never block the UI thread on git I/O.
- **Fidelity:** respect user's `~/.gitconfig`, hooks, credential helpers, signing config, `includeIf`.
- **Safety:** never lose user work; every destructive action previewed and (where possible) undoable.
- **Memory:** stream and virtualize; don't materialize the full graph for huge repos.
- **Offline-first:** every non-remote feature works without a network.
- **Cross-platform:** macOS, Windows, Linux with native feel (file dialogs, keychain, shortcuts).

---

## 10. Suggested Rust architecture

### Workspace layout (UI-agnostic core so the toolkit stays swappable)

```
gitgui/
├─ crates/
│  ├─ core/        # repo model, operations, undo/op-log; no UI deps
│  ├─ graph/       # commit-graph layout (topo order + lane assignment), virtualization index
│  ├─ diff/        # diff/hunk/line model, intra-line diff, patch building for partial staging
│  ├─ git-backend/ # trait Backend + impls (gix reads, git CLI for porcelain mutations)
│  ├─ services/    # fs watcher, auto-fetch, hosting-provider APIs (GitHub/GitLab), keychain
│  └─ app/         # GUI shell (egui / iced / gpui) — thin, talks to core via commands + events
```

### Key design decisions

1. **Hybrid git backend.**
   - Reads (log walk, refs, status, trees, blame, diff): in-process via `gix` (gitoxide) or `git2` (libgit2) for speed.
   - Mutating porcelain (rebase, interactive rebase, pull, push, merge with hooks, LFS, signing): shell out to the real `git` binary.
     This guarantees behavior matches the user's config, hooks, and credential helpers (lazygit takes this approach).
   - Hide it all behind a `Backend` trait so parts can migrate to pure-Rust as gitoxide coverage grows.
     **Check current gitoxide support for push / rebase / merge before committing to the split.**
2. **Command/event architecture.** UI sends `Command`s; a background worker (tokio or a thread pool) executes them and emits `Event`s
   (`RepoChanged`, `OperationProgress`, `OperationFailed`). UI state is a pure projection of core state.
3. **Operation log for undo.** Before each mutating op, record `(op, refs snapshot)`; undo = restore refs
   (reflog-compatible). This gives GitUp/jj-style global undo cheaply.
4. **Graph engine.** Topological order (use commit-graph file generation numbers when present) → lane assignment
   (gitk/gitg-style column allocation, stable colors per lane) → store as compact row records →
   render only visible rows. Load history in chunks; recompute only affected rows on ref changes.
5. **Live updates.** `notify` crate watches `.git/` (refs, HEAD, index) and the worktree, debounced → re-diff status, refresh graph incrementally.
6. **Partial staging.** Build a patch from selected lines/hunks and apply to the index
   (`git apply --cached`, or write the blob directly via the backend).

### GUI toolkit options (pure-Rust focus)

| Toolkit | Pros | Cons |
|---|---|---|
| **egui / eframe** | Fastest to iterate; trivial custom painting (great for the lane graph); mature | Immediate-mode quirks; text/a11y less polished; pixel-perfect native look is hard |
| **iced** | Elm-style architecture fits command/event design; canvas widget; used in COSMIC | Smaller widget ecosystem; some text-editing gaps |
| **GPUI** (Zed) | GPU-accelerated, excellent text rendering, built for editor-like UIs | API churn, sparse docs outside Zed |
| **Slint** | Declarative, polished, small footprint | Licensing terms to check; custom graph rendering needs more work |
| **Tauri / Dioxus** (webview) | Web ecosystem for diff/merge editors (Monaco/CodeMirror) | Not a native Rust UI; heavier runtime |

**Default recommendation (superseded by section 11c):** egui was the pick for the fastest MVP. After finding the Ely GPUI component library,
**GPUI + Ely** is now the preferred Rust-native option on macOS. Either way, keep `core`/`graph`/`diff` toolkit-agnostic.
These frameworks evolve quickly — recheck current state before locking in.

### Useful crates

`gix`, `git2`, `notify`, `tokio`, `similar` or `imara-diff` (diffing), `syntect` or `tree-sitter-highlight` (syntax), `keyring`,
`octocrab` (GitHub), `rfd` (native dialogs), `serde`/`toml` (config), `tracing`, `directories`, `ignore`.

---

## 11. Milestones

| M | Goal | Contents |
|---|---|---|
| **M0** | Skeleton | Workspace, `Backend` trait, open repo, list refs, headless CLI printing the log |
| **M1** | Graph viewer | Lane layout, virtualized scrolling, ref labels, commit detail + diff (read-only) |
| **M2** | Daily driver | Status, stage/unstage (file → hunk → line), commit/amend, branches, checkout, fetch/pull/push, stash |
| **M3** | Rewrite & resolve | Merge, rebase, interactive rebase editor, cherry-pick, revert, reset, 3-way conflict editor, tags |
| **M4** | Safety | Op-log + global undo/redo, reflog recovery, command log, destructive-action previews |
| **M5** | Polish & reach | Search/filters, blame, file history, command palette, submodules/worktrees/LFS, themes, auth for hosting providers |
| **M6** | Differentiators | PR integration, minimap, plugins (WASM?), custom commands, AI commit messages, jj-style conflict model |

## 11b. Platform decision: pure-Rust GUI vs. native macOS

| | A. Pure Rust GUI (egui/iced/GPUI) | B. Native macOS UI + Rust core (UniFFI) | C. Pure Swift (SwiftUI/AppKit) |
|---|---|---|---|
| Looks/feels like a Mac app | Weak–medium (custom-drawn widgets) | **Best** | **Best** |
| Text rendering, IME, accessibility, VoiceOver | Weak spots | Native (TextKit 2) | Native (TextKit 2) |
| Cross-platform later | **Yes, same UI code** | Core reusable; UI rewritten per OS | No |
| Huge-repo performance (graph layout, diff) | **Best** | **Best** (core in Rust) | Good if written carefully |
| macOS integration (Keychain, Touch ID, menus, drag-drop, Quick Look, Finder) | Manual/FFI per feature | Free | Free |
| Build complexity | Low (cargo only) | **High** (cargo → xcframework → Xcode, FFI, async bridging) | Low (Xcode only) |
| Time to a polished MVP | Medium | Slowest | **Fastest** |
| Git backend options | gix / git2 / CLI | gix / git2 / CLI | libgit2 (C), or CLI + own parser |
| Fit with "we build in Rust" | Yes | Yes (core) | No |

### Notes
- **Distribute outside the Mac App Store** (notarized DMG), as Fork/Tower/GitKraken do. The App Store sandbox
  blocks spawning `git` and reading arbitrary repo folders.
- macOS ships `git` as a shim that needs Xcode Command Line Tools. Detect it on first launch or bundle a git.
- SwiftUI `List` struggles with 100k+ rows. Use `NSTableView`/`NSCollectionView` or a custom Metal/CoreGraphics view for the
  commit graph, regardless of option B or C.
- In option B keep the FFI surface small and coarse-grained: `open_repo`, `graph_chunk(range)`, `status`,
  `diff(file)`, `run(Command)`, plus an event stream. Stream graph rows in chunks of compact structs. Do not chat across the boundary per row.

### Recommendation
- **macOS-only, ship fast:** C, pure Swift.
- **macOS-first, but want Rust speed and a path to Windows/Linux:** **B**, Rust core + SwiftUI/AppKit shell.
  The `core`/`graph`/`diff`/`git-backend` crates in section 10 stay as-is; only `app/` changes.
- **Cross-platform from day one, accept non-native feel:** A.

## 11c. Option D: GPUI + Ely component library (found via elygpui.com)

**What it is:** [Ely](https://elygpui.com) ([repo](https://github.com/ZacharyZhang-NY/Ely-GPUI-Components), MIT) is a component library for
GPUI, the GPU-accelerated Rust UI framework behind Zed. 1,279 components, light/dark themes, i18n (en/zh), runs on macOS, Windows and Linux
(CI covers all three), and each component also runs live in the browser via WASM. It has a dedicated **Git chapter** (`src/git/`).
Its own README says status is **early**.

**What I confirmed (from `src/git/mod.rs` exports, `graph.rs`, and the DiffViewer demo page):**

| Ely item | Covers (spec section) | Still ours to build |
|---|---|---|
| `DiffViewer`, `DiffLayout`, `diff`/`pairs`/`stat`, `DiffLine`, `LineKind`, `Stretch` (unified + split, word marks, folds) | §3 diff views, intra-line diff | Syntax highlighting, hunk/line staging logic, image diff |
| `graph::lanes` → `GraphRow`/`Stroke`/`Half` | §2 lane layout | Virtualization, incremental/chunked layout, stable lane colors, 100k–1M-commit performance |
| `CommitList`/`CommitItem`/`Commit` | §2 history list | Search/filters, ref labels wiring, lazy loading |
| `BlameView`/`Blame`/`GitBlameAnnotation`, `FileHistory` | §2 blame, file history | Backend blame data |
| `ThreeWayMerge`/`ConflictResolver`, `merging::{conflicts, regions, resolve, result, Take}` | §5 3-way merge editor | Wiring to merge/rebase state, continue/abort |
| `ChangesList`/`Changed`/`ChangeAction`/`CommitInput` | §3 status list + commit box | Real stage/unstage/discard behavior |
| `BranchList`/`BranchSelector`, `TagList`, `StashList` | §4 branches/tags, §3 stash | All git operations |
| `PullRequestCard`, `ReviewComment`, `ReviewNote` | §6 PR UI | Provider APIs (GitHub/GitLab) |
| `GitStatusBadge`, `DiffStat` | status/diff badges | — |
| Other chapters: terminal, editor, command-palette strings, tables, charts, toasts incl. `UndoToast`, collab comment sidebar | §8 terminal, palette, notifications | — |

**Not seen in the exports (verify before relying on it):** interactive-rebase editor, operation log / global undo, submodules, worktrees, LFS, auth,
any git backend. Ely is a **view layer**; it takes data you feed it (commits with parent ids, diffs, conflict text). Backend and undo stay ours (sections 10, 7).

**What I did not verify:** I read `mod.rs` and `graph.rs`, not every file, and did not run the library. The `graph.rs` layout clones its lane state
per row and lays out the whole input at once, so a 1M-commit repo needs a chunked/incremental variant. It is a good starting algorithm, not the final one.

**Risks:**
- Ely pins `gpui` and `gpui_platform` to a specific Zed git commit, so you take Zed's API churn.
- GPUI accessibility and IME are less mature than AppKit.
- Early-stage library with a small contributor base (5 contributors, 567 stars when checked).

**Effect on the platform decision:** this makes a Rust-native UI (option A) much stronger than egui/iced. GPUI renders text well and is built for
editor-like UIs; Zed is the existence proof on macOS. A GPUI + Ely UI could save weeks on diff, merge, blame and branch panels.

**Updated recommendation:** macOS-first and Rust-all-the-way → **D (GPUI + Ely UI, own core crates)**. Choose B/C only if native AppKit
accessibility and look matter more than staying in one language.

## 12. Open questions for you

1. Which platforms matter first (macOS only to start, or all three)?
2. Pure-Rust UI only, or is a webview-based UI (Tauri) acceptable if it speeds up diff/merge editors?
3. Single binary shelling out to system `git` (simplest), or fully self-contained (no `git` dependency)?
4. Positioning: "Fork clone, but faster", "GitUp for the modern era (undo-first)", or "keyboard-first lazygit with a graph"?
