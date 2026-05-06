"""
tray.py — Standalone GTK 3 tray indicator process.

This script is launched as a child process by the main GTK 4 app.
It communicates with the parent via stdin/stdout:
  - Reads lines from stdin: "quit\n"
  - Writes lines to stdout: "show\n" | "quit\n"

Run directly: python -m clipbrain.tray
"""

from __future__ import annotations

import sys

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("AyatanaAppIndicator3", "0.1")

from gi.repository import AyatanaAppIndicator3, GLib, Gtk


def _on_stdin(channel, condition, indicator):
    """Read commands sent from the parent process."""
    line = channel.readline()
    if not line or line.strip() == "quit":
        Gtk.main_quit()
        return False
    return True


def main():
    def send(msg: str):
        sys.stdout.write(msg + "\n")
        sys.stdout.flush()

    def on_show(_item):
        send("show")

    def on_quit(_item):
        send("quit")
        Gtk.main_quit()

    # Build menu
    menu = Gtk.Menu()

    show_item = Gtk.MenuItem(label="Show History")
    show_item.connect("activate", on_show)
    menu.append(show_item)

    menu.append(Gtk.SeparatorMenuItem())

    quit_item = Gtk.MenuItem(label="Quit")
    quit_item.connect("activate", on_quit)
    menu.append(quit_item)

    menu.show_all()

    # Build indicator
    try:
        indicator = AyatanaAppIndicator3.Indicator.new(
            "clipboard-tool",
            "edit-paste-symbolic",
            AyatanaAppIndicator3.IndicatorCategory.APPLICATION_STATUS,
        )
        indicator.set_status(AyatanaAppIndicator3.IndicatorStatus.ACTIVE)
        indicator.set_menu(menu)
    except Exception as exc:
        print(f"tray: indicator failed: {exc}", file=sys.stderr)
        sys.exit(1)

    # Watch stdin for commands from the parent
    stdin_channel = GLib.IOChannel.unix_new(sys.stdin.fileno())
    GLib.io_add_watch(stdin_channel, GLib.IOCondition.IN | GLib.IOCondition.HUP,
                      _on_stdin, indicator)

    Gtk.main()


if __name__ == "__main__":
    main()
