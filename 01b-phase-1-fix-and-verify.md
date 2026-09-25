# Phase 1b — Fix the Ubuntu environment and verify Phase 1

Review of your Phase 1 report: the code side looks good (lint, format and build pass, tasks implemented). Phase 1 is not done until the app actually runs on Ubuntu. Do only the following.

## 1. Fix apt (the user runs the sudo commands, you prepare them)

- Run `sudo apt update 2>&1 | tail -20` (or ask the user to run it and paste the output) and find the exact error about the conflicting Claude Desktop repository signing key (usually "Conflicting values set for option Signed-By").
- List the entries involved: `grep -rn -i claude /etc/apt/sources.list /etc/apt/sources.list.d/`
- Explain to the user in 2-3 short lines which file is the duplicate, and give the exact single command to remove or fix ONLY that duplicate entry (keep one working Claude Desktop entry; do not touch any other repository). Wait for the user to run it and confirm `sudo apt update` finishes without errors.

## 2. Install the missing Tauri dependencies

Give the user this command to run (adjust only if a package name does not exist on this Ubuntu version):

```
sudo apt install -y libwebkit2gtk-4.1-dev librsvg2-dev libxdo-dev libayatana-appindicator3-dev build-essential curl wget file libssl-dev
```

## 3. Git identity

Replace the placeholder identity `LensLate <lenslate@localhost>` for this repository: ask the user for their name and email and set them with `git config user.name` / `git config user.email` (local to this repo). Do not rewrite existing commits.

## 4. Verify

Run `cargo check`, `cargo clippy -- -D warnings` (in src-tauri) and fix any errors. Then start `pnpm tauri dev` and guide the user through this checklist, one item at a time, asking them to confirm each:

- [ ] Tray icon appears, no window at startup.
- [ ] `lenslate --toggle` (in dev: the equivalent command you provide) shows and hides the frame.
- [ ] Ctrl+Alt+Y: does it work on this Wayland session? (Report whether the GlobalShortcuts portal worked.)
- [ ] Frame can be dragged and resized; the translation bar follows it.
- [ ] Hover toolbar appears/disappears; close hides the frame; settings button opens Settings.
- [ ] Clicks inside the transparent area pass through (yes/no).
- [ ] Switching Ubuntu between light and dark changes the colours.
- [ ] Tray menu: Show/Hide, Settings, History, Quit all work.
      Fix every item that fails before reporting. Commit after each fix.

## 5. Report

Send the Phase report in the same format as before, with the checklist results.
