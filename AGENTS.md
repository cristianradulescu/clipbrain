# AGENTS.md — clipbrain

Clipboard history manager for GNOME on Wayland. Rust 2024 + gtk4-rs + libadwaita, single binary. Keep this file in sync with `CLAUDE.md`.

## Build / run / verify

```bash
cargo run                                     # CLIPBRAIN_LOG=debug for logs
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
./install.sh                                  # full install to ~/.local/bin/clipbrain

# Headless UI test (ignored by default; needs a display)
gtk4-broadwayd :9 &
GDK_BACKEND=broadway BROADWAY_DISPLAY=:9 cargo test -- --ignored
```

Single instance (`app.clipbrain`): a second launch only activates the running one. To run a dev build alongside an installed instance: `XDG_DATA_HOME=/tmp/cb dbus-run-session -- cargo run` (no tray on a private bus).

Build deps (Debian/Ubuntu): `libgtk-4-dev` (GTK ≥ 4.22), `libadwaita-1-dev` (≥ 1.9), `pkg-config`, `build-essential`. Crate features `v4_22` / `v1_9` are pinned to these.

## Architecture

One process: GTK on the main thread, tray on ksni's thread, file I/O and hashing on `gio::spawn_blocking`.

| File | Role |
|------|------|
| `src/main.rs` | Sets `GDK_BACKEND=x11`, logging, registers GResource, runs the app |
| `src/app.rs` | `AdwApplication` subclass; wires monitor, window, tray; `with_store()` for off-thread storage calls |
| `src/storage.rs` | `Store`: JSON index + PNGs, atomic writes, dedup, 200 cap; no GTK; unit-tested |
| `src/monitor.rs` | `ClipboardSource` trait + `GdkClipboardSource`; `Monitor` (`changed` signal + 500 ms poll) |
| `src/entry.rs` | `ClipEntry` GObject; off-thread 48 px thumbnails |
| `src/window/` | `HistoryWindow` + `EntryRow` (templates in `resources/*.ui`); hides on close |
| `src/tray.rs` | `ksni` StatusNotifierItem; events → `async_channel` → main loop |
| `resources/` | `.ui` templates, icon, `.desktop`; compiled by `build.rs` |

## Critical behaviour notes

### GNOME/Wayland clipboard capture
**The app runs under XWayland (`GDK_BACKEND=x11`, set in `main.rs`).** On native Wayland, mutter only delivers clipboard updates to the focused client, so copies made while the window is hidden are missed. X11 clients get every selection change. Don't remove this without replacing it with another background capture path.

`gdk::Clipboard`'s `changed` signal is coalesced by mutter: if one app copies several items rapidly, only the last fires the signal. **The 500 ms poll in `monitor.rs` is intentional and necessary**; do not remove it.

### `last_hash` dedup
The monitor keeps `last_hash` to suppress the spurious `changed` signals GNOME fires on focus changes. It is seeded from `Store::seed_hash()`. `Monitor::reset_last_hash()` must run after clearing history (wired via the window's `history-cleared` signal), otherwise re-copying the same text is silently dropped.

Clipboard changes made by clipbrain itself (`is_local()`) are ignored; copying an entry promotes it in storage explicitly.

### Window lifecycle
`HistoryWindow` is created once and hidden/shown, never re-created. `show_window()` always calls `reload()` before `present()` to pick up entries captured while hidden. Captures only trigger `reload()` while the window is visible.

The capture-phase key handler ignores list keys while a dialog is open, and passes `j`/`k`/`?`/`Delete` through while the search entry is focused.

## Storage
- `~/.local/share/clipbrain/history.json`: JSON array, newest first, max 200 entries, 2-space indent, Python-`isoformat()` UTC timestamps
- `~/.local/share/clipbrain/images/<uuid>.png`, referenced by absolute `image_path`
- Text deduplicated by content (move-to-front); images by `content_hash` (SHA-256 hex of the PNG bytes)
- Owner-only permissions (0700 dirs, 0600 files); atomic writes; `Store` is the only writer while the app runs
- Format is compatible with the previous Python version (see `tests/fixtures/`)
