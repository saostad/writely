# Writely
Writely helps you write grammatically correct English with Google Gemini.
Run it with `cargo run`.
On macOS, Writely needs Accessibility permission to read the focused text field.
Open Settings from the tray icon to add your Gemini API key.

## Run on macOS
First run `scripts/create-signing-cert-macos.sh` once so macOS keeps Writely's Accessibility permission across rebuilds. Then build the app bundle with `scripts/bundle-macos.sh`, then run `open target/release/Writely.app` (or drag it to Applications first, then open it from there). Turn on Writely in System Settings › Privacy & Security › Accessibility; Writely starts working by itself once the switch is on, no restart needed.
Note: without the signing certificate the build is ad-hoc signed, so macOS may forget the Accessibility permission after each rebuild. If Writely stops seeing the cursor, turn its switch off and on again (or remove it with − and reopen Writely).
