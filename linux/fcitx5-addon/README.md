# Writely bridge add-on for fcitx5

A fcitx5 module add-on that connects fcitx5 input contexts to the Writely
process over the session D-Bus, so Writely works in native Wayland sessions
where it can't see the screen or place its own windows.

Protocol reference: `docs/linux-ime-bridge.md` in the Writely repo. The
context id is the input context UUID as lowercase hex.

## Build

Needs fcitx5 5.1 development files and pkg-config (Ubuntu:
`libfcitx5core-dev libfcitx5utils-dev libfcitx5config-dev`, plus the module
dev files; Arch: `fcitx5`):

```sh
make -C linux/fcitx5-addon
```

This builds `libwritely.so` with `-Wall -Wextra -Werror` (C++20).

## Install

Per user (no root needed):

```sh
make -C linux/fcitx5-addon install-user
```

Then restart fcitx5 with the printed `FCITX_ADDON_DIRS=...` command line so it
picks up `~/.local/lib/fcitx5/`.

System-wide:

```sh
sudo make -C linux/fcitx5-addon install
```

(`PREFIX`, `DESTDIR` and `LIBDIR` can be overridden; `LIBDIR` defaults to
`pkg-config --variable=libdir Fcitx5Core`.)

## Behavior

- Watches `dev.writely.Writely` on the session bus. While Writely isn't
  running the add-on stays idle and logs one line; when Writely appears it
  says `Hello` and reports the focused field.
- Reports `FocusIn`/`FocusOut`/`SurroundingText`/`Destroy` per input context.
  All D-Bus calls are async; failures are logged with the D-Bus error.
- Shows Writely's "W" icon and suggestion messages in fcitx5's input panel at
  the cursor, without touching another engine's panel content, and applies
  accepted fixes via surrounding-text deletion.
- For apps that don't share their text (e.g. Chromium), reports
  `TextUnavailable` and falls back to selecting the line with `Shift+Home`
  and reading it through the PRIMARY selection (the clipboard is untouched);
  accepted fixes are committed over the still-active selection.
