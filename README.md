# Clipbrain

A clipboard history manager for GNOME on Wayland, built with Python, GTK4, and libadwaita.

![Clipbrain screenshot](./screenshot.png "Clipbrain screenshot")

## Features

- **Text and image history** — captures everything you copy, up to 200 entries
- **Deduplication** — re-copying existing content moves it to the top rather than duplicating it
- **Search** — filter history instantly with Ctrl+F
- **System tray** — runs in the background; window hides on close
- **Persistent** — history survives restarts

## Requirements

- GNOME on Wayland
- Python 3.12+
- Debian/Ubuntu-based system (apt) — the installer handles everything else

## Installation

```bash
git clone <repo-url>
cd clipbrain
./install.sh
```

`install.sh` will:
1. Install missing system libraries via `apt`
2. Install [uv](https://docs.astral.sh/uv/) if not already present
3. Set up the Python environment (`uv pip install -e .`)
4. Install `~/.local/bin/clipbrain`
5. Optionally register a GNOME keyboard shortcut and autostart entry

## Running

```bash
clipbrain
```

Or without installing:

```bash
uv run clipbrain
```

## Usage

Press **Ctrl+Shift+V** or click the tray icon to open the history window. Click any entry to copy it back to the clipboard.

## Keyboard Shortcuts

| Key | Action |
|---|---|
| `Ctrl+Shift+V` | Show history window |
| `↓` / `j` | Select next item |
| `↑` / `k` | Select previous item |
| `Enter` | Copy selected item to clipboard |
| `Delete` | Delete selected item |
| `Ctrl+F` / `/` | Search history |
| `Escape` | Close search |
| `?` / `F1` | Show keyboard shortcuts |
| `Ctrl+Shift+Delete` | Clear all history |

## Architecture

| File | Role |
|---|---|
| `clipbrain/main.py` | Entry point |
| `clipbrain/app.py` | `Adw.Application`; wires monitor, window, tray |
| `clipbrain/monitor.py` | Clipboard watcher; `changed` signal + 500 ms poll |
| `clipbrain/storage.py` | JSON index + PNG files in `~/.local/share/clipbrain/` |
| `clipbrain/window.py` | `Adw.ApplicationWindow`; hides on close, never destroyed |
| `clipbrain/tray.py` | Separate GTK3 process for `AyatanaAppIndicator3` tray icon |
| `install.sh` | Full installer |

## License

MIT
