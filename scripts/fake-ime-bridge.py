#!/usr/bin/env python3
"""Fake Linux input method bridge for trying Writely without IBus/fcitx5.

Keeps one D-Bus connection, sends Hello/FocusIn/SurroundingText, prints the
signals Writely emits, and answers Replace. See docs/linux-ime-bridge.md.
Needs python3 with PyGObject (gi.repository.Gio/GLib) only.
"""

import argparse
import sys

import gi

gi.require_versions({"Gio": "2.0", "GLib": "2.0"})
from gi.repository import Gio, GLib  # noqa: E402

BUS_NAME = "dev.writely.Writely"
OBJECT_PATH = "/dev/writely/ImeBridge"
INTERFACE = "dev.writely.ImeBridge1"
CONTEXT = "fake1"
DEFAULT_TEXT = "Yesterday I goes to the store."


def call(connection, method, params):
    try:
        return connection.call_sync(
            BUS_NAME, OBJECT_PATH, INTERFACE, method, params,
            None, Gio.DBusCallFlags.NONE, -1, None,
        )
    except GLib.Error as error:
        print(f"fake-ime-bridge: {method} failed: {error}", file=sys.stderr)
        sys.exit(1)


def main():
    parser = argparse.ArgumentParser(description="Fake Writely IME bridge.")
    parser.add_argument("text", nargs="?", default=DEFAULT_TEXT,
                        help="surrounding text to send")
    parser.add_argument("--click-check", action="store_true",
                        help="send Clicked check once after the first ShowIcon")
    parser.add_argument("--apply", action="store_true",
                        help="send Clicked apply once after a ShowMessage with can_apply=True")
    parser.add_argument("--no-text", action="store_true",
                        help="send TextUnavailable instead of SurroundingText, "
                             "and answer SelectRequest/ReplaceSelection")
    parser.add_argument("--timeout", type=int, default=30,
                        help="seconds before Destroy and exit 0 (default 30)")
    args = parser.parse_args()

    state = {
        "text": args.text,
        "click_check_sent": False,
        "apply_sent": False,
    }
    try:
        connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    except GLib.Error as error:
        print(f"fake-ime-bridge: bus failed: {error}", file=sys.stderr)
        sys.exit(1)

    def on_signal(_conn, _sender, _path, _iface, signal, params, _data):
        values = params.unpack()
        if signal == "ShowIcon":
            (context,) = values
            print(f"signal ShowIcon {context}", flush=True)
            if args.click_check and not state["click_check_sent"]:
                state["click_check_sent"] = True
                call(connection, "Clicked", GLib.Variant("(ss)", (context, "check")))
        elif signal == "ShowMessage":
            (context, text, is_error, can_apply) = values
            print(f"signal ShowMessage {context} is_error={is_error} "
                  f"can_apply={can_apply}: {text}", flush=True)
            if args.apply and can_apply and not state["apply_sent"]:
                state["apply_sent"] = True
                call(connection, "Clicked", GLib.Variant("(ss)", (context, "apply")))
        elif signal == "Hide":
            (context,) = values
            print(f"signal Hide {context}", flush=True)
        elif signal == "Replace":
            (context, request, start, end, text) = values
            print(f"signal Replace {context} request={request} "
                  f"start={start} end={end}: {text}", flush=True)
            current = state["text"]
            state["text"] = current[:start] + text + current[end:]
            print(f"text now: {state['text']}", flush=True)
            call(connection, "ReplaceResult",
                 GLib.Variant("(subs)", (context, request, True, "")))
            cursor = len(state["text"])
            call(connection, "SurroundingText",
                 GLib.Variant("(ssuu)", (context, state["text"], cursor, cursor)))
        elif signal == "SelectRequest":
            (context, request) = values
            print(f"signal SelectRequest {context} request={request}", flush=True)
            call(connection, "Selected",
                 GLib.Variant("(subss)", (context, request, True, state["text"], "")))
        elif signal == "ReplaceSelection":
            (context, request, text) = values
            print(f"signal ReplaceSelection {context} request={request}: {text}",
                  flush=True)
            print(f"selection replaced with: {text}", flush=True)
            call(connection, "ReplaceResult",
                 GLib.Variant("(subs)", (context, request, True, "")))
        else:
            print(f"signal {signal} {values}", flush=True)

    connection.signal_subscribe(
        None, INTERFACE, None, OBJECT_PATH, None,
        Gio.DBusSignalFlags.NONE, on_signal, None,
    )

    call(connection, "Hello", GLib.Variant("(ss)", ("fake", "1")))
    call(connection, "FocusIn",
         GLib.Variant("(ssb)", (CONTEXT, "fake-ime-bridge", False)))
    if args.no_text:
        call(connection, "TextUnavailable",
             GLib.Variant("(sb)", (CONTEXT, True)))
    else:
        cursor = len(state["text"])
        call(connection, "SurroundingText",
             GLib.Variant("(ssuu)", (CONTEXT, state["text"], cursor, cursor)))

    loop = GLib.MainLoop()

    def on_timeout():
        call(connection, "Destroy", GLib.Variant("(s)", (CONTEXT,)))
        loop.quit()
        return GLib.SOURCE_REMOVE

    GLib.timeout_add_seconds(args.timeout, on_timeout)
    loop.run()
    return 0


if __name__ == "__main__":
    sys.exit(main())
