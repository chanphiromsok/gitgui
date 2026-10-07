## Install

**macOS** — download the `.dmg` for your Mac: `arm64` for Apple Silicon (M1 and later), `x86_64` for Intel. Open it and drag gitgui to Applications.
These builds are not notarized by Apple, so the first launch needs one of:
- right-click gitgui in Applications → **Open** → **Open**, or
- `xattr -dr com.apple.quarantine /Applications/gitgui.app`

**Windows** — download the `.zip`, unzip it, and run `gitgui.exe`. Windows SmartScreen may warn because the file is not signed: **More info → Run anyway**. Git for Windows must be installed (gitgui runs the `git` command).

Both need `git` on the PATH. Check the `SHA256SUMS` file to verify a download.
