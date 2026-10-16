# AGENTS.md — clipbrain

## Run / develop

```bash
uv run clipbrain        # run from repo root (required — uses the venv)
```

No test suite, no lint config, no type-check config. Verification = run the app and exercise the UI manually.

## Architecture

Single GTK4 process + one GTK3 child subprocess:

| File | Role |
|------|------|
| `main.py` | Entry point, starts `ClipboardApp` |
| `app.py` | `Adw.Application`; wires monitor, window, tray |
| `monitor.py` | Clipboard watcher; `changed` signal + 500 ms poll |
| `storage.py` | JSON index + PNG files in `~/.local/share/clipbrain/` |
| `window.py` | `Adw.ApplicationWindow`; hides on close (never destroyed) |
| `tray.py` | **Separate process** — GTK3 + `AyatanaAppIndicator3`; communicates via stdin/stdout lines (`show`, `quit`) |

**Why two processes:** `AyatanaAppIndicator3` requires GTK3. GTK3 and GTK4 (`Adw`) cannot coexist in the same process.

## Critical behaviour notes

### GNOME/Wayland clipboard coalescing
**The app runs under XWayland (`GDK_BACKEND=x11`, set in `main.py`).** On native Wayland, mutter only delivers clipboard updates to the focused client, so copies made while the window is hidden are missed. X11 clients get every selection change. Don't remove this without replacing it with another background capture path.

`Gdk.Clipboard`'s `changed` signal is fired by mutter and is coalesced — if the same app copies several items rapidly, only the last fires the signal. **The 500 ms polling loop in `monitor.py` is intentional and necessary**; do not remove it.

### `_last_hash` dedup
The monitor keeps `_last_hash` to suppress the spurious `changed` signal that GNOME fires on focus changes. Call `monitor.reset_last_hash()` (already wired via `on_history_cleared` callback) after clearing history, otherwise re-copying the same text is silently dropped.

### Window lifecycle
`HistoryWindow` is created once and hidden/shown — never re-created. `_show_window()` always calls `reload()` before `present()` to pick up entries captured while the window was hidden.

### `reload()` vs live update
`_on_new_text` / `_on_new_image` in `app.py` call `window.reload()` only when the window is visible. When hidden, entries accumulate in storage and `reload()` on show picks them all up.

## Storage
- `~/.local/share/clipbrain/history.json` — JSON array, newest first, max 200 entries
- `~/.local/share/clipbrain/images/` — PNG files named by UUID
- Text entries deduplicated by content (move-to-front); images by `content_hash` (SHA-256 in JSON)

## System dependencies (Debian/Ubuntu)
The Python `pygobject` wheel requires native GObject introspection libs. Install via:
```bash
./install.sh
```
or manually:
```
libgirepository-2.0-dev libgtk-4-dev gir1.2-gtk-4.0 libadwaita-1-dev gir1.2-adw-1
libgtk-3-dev gir1.2-gtk-3.0 libayatana-appindicator3-1 gir1.2-ayatanaappindicator3-0.1
python3-gi python3-gi-cairo python3-dev
```
`pygobject` must be installed inside the uv venv (`uv pip install -e .`), not from apt.
