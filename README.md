# Writely
Writely helps you write grammatically correct English with Google Gemini.
Run it with `cargo run`.
On macOS, Writely needs Accessibility permission to read the focused text field.
Open Settings from the tray icon to add your Gemini API key.

## Run on macOS
First run `scripts/create-signing-cert-macos.sh` once so macOS keeps Writely's Accessibility permission across rebuilds. Then build the app bundle with `scripts/bundle-macos.sh`, then run `open target/release/Writely.app` (or drag it to Applications first, then open it from there). Turn on Writely in System Settings › Privacy & Security › Accessibility; Writely starts working by itself once the switch is on, no restart needed.
Note: without the signing certificate the build is ad-hoc signed, so macOS may forget the Accessibility permission after each rebuild. If Writely stops seeing the cursor, turn its switch off and on again (or remove it with − and reopen Writely).

## Download

Get the latest installer from the Releases section of the GitHub repo. Every push to
the `main` branch builds the installers and creates a new release
(version `<major>.<minor>.<run-number>`), so the list keeps growing. The builds are
unsigned, so expect a warning on first run:

- macOS (`.dmg`): right-click the app › Open the first time. Accessibility must be
  re-granted after each update, because ad-hoc builds get a new identity.
- Windows (`.exe` installer): SmartScreen shows "More info › Run anyway".
- Linux (`.deb` or `.AppImage`): X11 only for the caret icon; on Wayland use an X11 session.
