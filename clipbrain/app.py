"""
app.py — ClipboardApp (Adw.Application).

The tray icon runs in a separate child process (clipbrain/tray.py)
using GTK 3 + AyatanaAppIndicator3.  The two processes communicate via
stdin/stdout pipes:
  parent → child : "quit\\n"
  child  → parent: "show\\n" | "quit\\n"
"""

from __future__ import annotations

import os
import subprocess
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")

from gi.repository import Adw, GLib, Gio

from clipbrain import storage
from clipbrain.monitor import ClipboardMonitor
from clipbrain.window import HistoryWindow


class ClipboardApp(Adw.Application):
    def __init__(self) -> None:
        super().__init__(
            application_id="app.clipbrain",
            flags=Gio.ApplicationFlags.DEFAULT_FLAGS,
        )
        self._window: HistoryWindow | None = None
        self._monitor: ClipboardMonitor | None = None
        self._tray_proc: subprocess.Popen | None = None
        self._tray_io_watch: int | None = None

        self.connect("activate", self._on_activate)
        self.connect("startup", self._on_startup)
        self.connect("shutdown", self._on_shutdown)

    # ------------------------------------------------------------------

    def _on_startup(self, _app) -> None:
        self.hold()

    def _on_activate(self, _app) -> None:
        if self._monitor is None:
            self._monitor = ClipboardMonitor(
                on_text=self._on_new_text,
                on_image=self._on_new_image,
            )
            self._monitor.start()

        if self._window is None:
            self._window = HistoryWindow(
                application=self,
                on_history_cleared=self._monitor.reset_last_hash,
            )
            self.set_accels_for_action("app.show-window", ["<Control><Shift>v"])
            show_action = Gio.SimpleAction.new("show-window", None)
            show_action.connect("activate", lambda *_: self._show_window())
            self.add_action(show_action)

        if self._tray_proc is None:
            self._start_tray_process()

        self._show_window()

    def _on_shutdown(self, _app) -> None:
        self._stop_tray_process()

    # ------------------------------------------------------------------
    # Clipboard callbacks
    # ------------------------------------------------------------------

    def _on_new_text(self, text: str) -> None:
        storage.add_text(text)
        if self._window is not None and self._window.get_visible():
            self._window.reload()

    def _on_new_image(self, png_bytes: bytes) -> None:
        storage.add_image(png_bytes)
        if self._window is not None and self._window.get_visible():
            self._window.reload()

    # ------------------------------------------------------------------
    # Window
    # ------------------------------------------------------------------

    def _show_window(self) -> None:
        if self._window is None:
            return
        self._window.reload()
        self._window.set_visible(True)
        self._window.present()

    def _quit(self) -> None:
        self._stop_tray_process()
        self.release()
        self.quit()

    # ------------------------------------------------------------------
    # Tray subprocess
    # ------------------------------------------------------------------

    def _start_tray_process(self) -> None:
        try:
            # Find the Python interpreter used by this process
            python = sys.executable
            self._tray_proc = subprocess.Popen(
                [python, "-m", "clipbrain.tray"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=None,
                text=True,
                bufsize=1,  # line-buffered
                cwd=os.path.dirname(os.path.dirname(__file__)),
            )
            # Watch the child's stdout on the GLib main loop
            channel = GLib.IOChannel.unix_new(self._tray_proc.stdout.fileno())
            self._tray_io_watch = GLib.io_add_watch(
                channel,
                GLib.PRIORITY_DEFAULT,
                GLib.IOCondition.IN | GLib.IOCondition.HUP,
                self._on_tray_message,
            )
        except Exception as exc:
            print(f"[clipboard-tool] could not start tray process: {exc}", file=sys.stderr)
            self._tray_proc = None

    def _on_tray_message(self, channel: GLib.IOChannel, condition: GLib.IOCondition) -> bool:
        if condition & GLib.IOCondition.HUP:
            self._tray_proc = None
            return False  # remove watch

        line = channel.readline()
        if not line:
            return True
        cmd = line.strip()
        if cmd == "show":
            self._show_window()
        elif cmd == "quit":
            self._quit()
        return True

    def _stop_tray_process(self) -> None:
        if self._tray_proc is not None:
            try:
                self._tray_proc.stdin.write("quit\n")
                self._tray_proc.stdin.flush()
            except Exception:
                pass
            try:
                self._tray_proc.wait(timeout=2)
            except Exception:
                self._tray_proc.kill()
            self._tray_proc = None
