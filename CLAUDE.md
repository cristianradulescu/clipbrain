# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Clipbrain is a clipboard history manager for GNOME on Wayland (Python 3.12+, GTK4, libadwaita). `AGENTS.md` covers the same ground; keep the two in sync.

## Commands

```bash
uv run clipbrain            # run from repo root (uses the uv venv; required)
uv pip install -e .         # (re)install into the venv after dependency changes
python -m clipbrain.tray    # run the GTK3 tray process standalone for debugging
./install.sh                # full install: apt deps, uv, venv, ~/.local/bin/clipbrain, optional GNOME shortcut + autostart
```

There is no test suite, linter, or type-checker configured. Verification means running the app and exercising the UI by hand. Because the app is a single-instance `Adw.Application` (`app.clipbrain`), launching it while it is already running just re-activates the existing instance and shows its window.

`pygobject` must be installed in the uv venv, not from apt; the native GObject/GTK libs come from apt (see `install.sh` for the exact package list).

## Architecture

One GTK4 process plus one GTK3 child process:

- `app.py` — `ClipboardApp` (`Adw.Application`). On activate it creates the `ClipboardMonitor`, the single `HistoryWindow`, and spawns the tray subprocess. Registers the `Ctrl+Shift+V` accelerator for `app.show-window`.
- `monitor.py` — `ClipboardMonitor` watches `Gdk.Clipboard`, calling `on_text` / `on_image` callbacks with the new content (images are normalised to PNG bytes).
- `storage.py` — module-level functions over `~/.local/share/clipbrain/history.json` (newest-first array, capped at `MAX_ENTRIES` = 200) plus `images/<uuid>.png`. Text is deduplicated by content, images by SHA-256 `content_hash`; re-adding moves the entry to the front.
- `window.py` — `HistoryWindow` (`Adw.ApplicationWindow`) with a `Gtk.ListView` over `EntryObject` wrappers, search filtering, and vim-style key handling.
- `tray.py` — **separate process** using GTK3 + `AyatanaAppIndicator3`, launched by `app.py` as `python -m clipbrain.tray` with cwd set to the repo root. GTK3 and GTK4 cannot coexist in one process, which is why the tray lives here. Protocol is line-based over pipes: parent → child `quit`; child → parent `show` | `quit`.

## Behaviour that looks wrong but is intentional

- **The app runs under XWayland (`GDK_BACKEND=x11`, set in `main.py`).** On native Wayland, mutter only delivers clipboard updates to the focused client, so copies made while the window is hidden are missed. X11 clients get every selection change. Don't remove this without replacing it with another background capture path.
- **500 ms polling in `monitor.py` is required.** Mutter coalesces `Gdk.Clipboard`'s `changed` signal, so rapid consecutive copies from one app fire only once. The poll catches the rest. Do not remove it.
- **`_last_hash` dedup in the monitor.** GNOME fires spurious `changed` signals on focus changes; the monitor suppresses them by hashing content. It is seeded from the top storage entry on startup so restarts don't re-store the current clipboard. After clearing history, `monitor.reset_last_hash()` must run (wired via the window's `on_history_cleared` callback) or re-copying the same text is silently dropped.
- **The window is never destroyed.** `HistoryWindow` is created once; closing hides it. `_show_window()` always calls `reload()` before `present()` so entries captured while hidden appear.
- **Live updates only when visible.** `_on_new_text` / `_on_new_image` in `app.py` call `window.reload()` only if the window is visible; otherwise entries accumulate in storage until the next show.
