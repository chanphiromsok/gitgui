## Install

**macOS** — download the `.dmg` for your Mac: `arm64` for Apple Silicon (M1 and later), `x86_64` for Intel. Open it and drag gitgui to Applications.
These builds are not notarized by Apple (that needs a paid Apple account), so macOS blocks the first launch. Do one of these, once:
- Terminal: `xattr -dr com.apple.quarantine /Applications/gitgui.app`, then open gitgui normally. This works on every macOS version.
- macOS 15 (Sequoia) and newer: double-click gitgui, press **Done**, then open **System Settings → Privacy & Security**, scroll down and press **Open Anyway** (it shows for about an hour after the attempt).
- macOS 14 and older: right-click gitgui → **Open** → **Open**.

**Windows** — download the `.zip`, unzip it, and run `gitgui.exe`. Windows SmartScreen may warn because the file is not signed: **More info → Run anyway**. Git for Windows must be installed (gitgui runs the `git` command).

Both need `git` on the PATH. Check the `SHA256SUMS` file to verify a download.
