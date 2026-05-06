"""
main.py — Entry point for clipboard-tool.

The main process runs GTK 4 + libadwaita.  The tray icon is handled by a
separate child process (clipbrain/tray.py) that uses GTK 3 +
AyatanaAppIndicator3, communicating with the parent via stdin/stdout pipes.
"""

import sys
from clipbrain.app import ClipboardApp


def main():
    app = ClipboardApp()
    sys.exit(app.run(sys.argv))


if __name__ == "__main__":
    main()
