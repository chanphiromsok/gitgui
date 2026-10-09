<div align="center">

# gitgui

**A fast, native Git client that understands pull requests.**

![Rust](https://img.shields.io/badge/Rust-GPUI-b7410e?style=flat-square)
![macOS and Windows](https://img.shields.io/badge/macOS%20%C2%B7%20Windows-lightgrey?style=flat-square)
![MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)
![No account](https://img.shields.io/badge/no%20account%20%C2%B7%20no%20telemetry-2ea44f?style=flat-square)

<br>

<img src="docs/img/hero.webp" alt="gitgui: a release branch with its pull requests grouped, a squash-merged branch marked as merged, and branches in their own colours" width="900">

</div>

<br>

## Why you'll like it

- **It knows what's merged.** Squash and rebase merges leave no trace in git, so other tools show the branch as open forever. gitgui finds them and marks the branch **✓ squash-merged**.
- **Pull requests, tidy.** A pull request's commits sit together under it and fold away.
- **It remembers your team's workflow.** Branch names, where work starts, how things merge: read from the repo, then **New branch…** builds `feature/74-driver-reporting` for you.
- **Conflicts, explained.** Resolve them without leaving the app, with each side named plainly (never "ours/theirs") and who changed it and why in front of you. The merge question tells you beforehand if it will conflict.
- **Blame where you're looking.** Rest the pointer on a line in a diff: who changed it, and when.
- **Where is it used?** Cmd-click a word in a diff: the files that use it in that commit, the line that declares it first, each one click from the whole file.
- **Yours to restyle.** 14 graph styles, each with its own lines, commit marks and labels.
- **Fast.** Native Rust, no Electron: thousands of commits and 10,000-line diffs scroll smoothly.

<br>

## See it

<img src="docs/img/review.webp" alt="A split diff with syntax colours and a blame note beside the line the pointer is on" width="900">

*Split diff, with who-changed-this one hover away. Comment on any line.*

<br>

<img src="docs/img/wizard.webp" alt="The New branch window, building feature/101-driver-reporting-v2 from develop" width="900">

*Start a branch the way your team does: click a type for its prefix (or skip it), add a ticket and a title. Start from opens a searchable list of local and remote branches.*

<br>

<img src="docs/img/styles.webp" alt="The same history in six graph styles: Neon, Soft, Circuit, Graphite, Tokyo Night and Gruvbox" width="900">

*Six of the 14 styles. Colours are fitted to your theme, so every line stays readable.*

<details>
<summary>Light theme</summary>
<br>
<img src="docs/img/light.webp" alt="gitgui in the One Light theme" width="900">
</details>

<br>

## Get it

**Download** for macOS (Apple silicon and Intel) or Windows from [Releases](https://github.com/chanphiromsok/gitgui/releases). It needs `git` on your PATH.

**Or build it** (Rust, stable):

```sh
cargo run --release -p gitgui-app -- /path/to/repo
```

<br>

## More

[Guide](docs/guide.md): everything it does · [Feature spec](docs/feature-spec.md): how it is built · MIT
