"""
main.py — Entry point for clipboard-tool.

The main process runs GTK 4 + libadwaita.  The tray icon is handled by a
separate child process (clipbrain/tray.py) that uses GTK 3 +
AyatanaAppIndicator3, communicating with the parent via stdin/stdout pipes.
"""

import os
import sys

# Run through XWayland.  On native Wayland, mutter only sends clipboard
# updates to the focused client, so with the window hidden copies are missed
# (or read only in whatever formats were on offer when we last had focus).
# X11 clients are notified of every selection change regardless of focus.
# Must be set before GTK is imported; an explicit GDK_BACKEND still wins.
os.environ.setdefault("GDK_BACKEND", "x11")

from clipbrain.app import ClipboardApp  # noqa: E402


def main():
    app = ClipboardApp()
    sys.exit(app.run(sys.argv))


if __name__ == "__main__":
    main()
