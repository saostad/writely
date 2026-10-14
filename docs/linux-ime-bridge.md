# Linux input method bridge protocol (v1)

Native Wayland support for Writely goes through the input method framework
(IBus on Ubuntu/GNOME, fcitx5 on Omarchy/Hyprland and many KDE setups). On
Wayland, apps can't learn screen positions and can't place windows at
coordinates, so the X11 code path (AT-SPI + GTK popups at caret coordinates)
doesn't work there. Instead, a thin bridge per framework talks to the Writely
process over the session D-Bus, and the bridge draws the "W" and the
suggestion in the framework's own input panel, which the compositor positions
at the cursor. X11 sessions keep the existing AT-SPI code unchanged.

This document is the reference for bridge authors. Writely implements the
service side; bridges call its methods and listen to its signals.

## Endpoint

| Item | Value |
|---|---|
| Bus | session bus |
| Well-known name | `dev.writely.Writely` (owned by Writely) |
| Object path | `/dev/writely/ImeBridge` |
| Interface | `dev.writely.ImeBridge1` |
| Protocol version | `1` (returned by `Hello`) |

## Contexts and offsets

A context is one text field the bridge is tracking, identified by the pair
(D-Bus unique sender name of the bridge, `context` string). Two bridges may
reuse the same `context` string; they never collide because the sender name
is part of the key. A context exists from its first `FocusIn` until its
bridge disappears from the bus.

All offsets (`cursor`, `anchor`, `start`, `end`) are counts of Unicode scalar
values (Rust `char`s) into the `text` of the latest `SurroundingText` for
that context. They are never byte offsets: in `"héllo 👋 wörld"`, the `👋`
is one offset unit.

`anchor != cursor` in `SurroundingText` means a selection between them, in
either order; `anchor == cursor` means no selection.

## Methods (bridges call these)

### `Hello(bridge: s, bridge_version: s) -> (protocol_version: u)`

Handshake. Returns `1`. Writely logs
`Writely: connected to the {bridge} input method bridge {bridge_version}.`

```sh
busctl --user call dev.writely.Writely /dev/writely/ImeBridge \
  dev.writely.ImeBridge1 Hello ss fcitx5 5.1
```

### `FocusIn(context: s, program: s, password: b)`

This context now has keyboard focus, replacing any previous focus from any
bridge. `program` is the focused app's name and may be empty. `password` is
true for password fields, which Writely never reads.

```sh
busctl --user call dev.writely.Writely /dev/writely/ImeBridge \
  dev.writely.ImeBridge1 FocusIn ssb ctx1 gedit false
```

### `FocusOut(context: s)`

Clears focus, but only if this (sender, context) is the focused one;
otherwise it is a no-op.

```sh
busctl --user call dev.writely.Writely /dev/writely/ImeBridge \
  dev.writely.ImeBridge1 FocusOut s ctx1
```

### `SurroundingText(context: s, text: s, cursor: u, anchor: u)`

Stores the text around the cursor for a context that had `FocusIn`.
`cursor` is the caret offset; `anchor` is the other end of the selection,
or equal to `cursor` when there is none.

```sh
busctl --user call dev.writely.Writely /dev/writely/ImeBridge \
  dev.writely.ImeBridge1 SurroundingText ssuu ctx1 'hello world' 5 5
```

### `Clicked(context: s, action: s)`

The user clicked something in the input panel. `action` is one of `check`,
`apply`, `dismiss`.

```sh
busctl --user call dev.writely.Writely /dev/writely/ImeBridge \
  dev.writely.ImeBridge1 Clicked ss ctx1 check
```

### `ReplaceResult(context: s, request: u, ok: b, error: s)`

Answer to a `Replace` signal: whether replacing `request` worked, with a
human-readable `error` when it didn't.

```sh
busctl --user call dev.writely.Writely /dev/writely/ImeBridge \
  dev.writely.ImeBridge1 ReplaceResult subs ctx1 1 true ''
```

## Signals (Writely emits these)

Signals are **unicast**: each one is sent with the D-Bus destination set to
the unique name of the bridge that owns the context, never broadcast.

- `ShowIcon(context: s)` — show the "W" in the input panel.
- `ShowMessage(context: s, text: s, is_error: b, can_apply: b)` — show text;
  `can_apply` true means an Apply action is available.
- `Hide(context: s)` — hide the panel.
- `Replace(context: s, request: u, start: u, end: u, text: s)` — replace
  chars `start..end` of that context's latest surrounding text with `text`.
  `request` increases by 1 per `Replace`, starting at 1; answer with
  `ReplaceResult`.

Watch them with e.g.:

```sh
busctl --user monitor dev.writely.Writely
```

## Errors

Invalid input is answered with the D-Bus error name
`dev.writely.ImeBridge1.Error.InvalidArgument` and a message that names the
method and the bad values (also printed to stderr as `Writely: ...`):

- cursor or anchor past the end:
  `SurroundingText: cursor {cursor} / anchor {anchor} is past the end of the {len}-character text.`
- unknown action:
  `Clicked: unknown action "{action}" (expected check, apply or dismiss).`
- `SurroundingText`/`Clicked`/`ReplaceResult` for a context that never had
  `FocusIn` from that sender:
  `{Method}: unknown context "{context}"; call FocusIn first.`

## Bridge disconnects

When a bridge disappears from the bus (`NameOwnerChanged` with an empty new
owner), Writely drops all of its contexts and clears focus if it owned the
focused one. A bridge that restarts just calls `FocusIn` again; no explicit
goodbye is needed.
