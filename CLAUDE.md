# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Clipbrain is a clipboard history manager for GNOME on Wayland (Rust 2024, gtk4-rs, libadwaita). `AGENTS.md` covers the same ground; keep the two in sync.

## Commands

```bash
cargo run                                     # run from the repo (CLIPBRAIN_LOG=debug for logs)
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
./install.sh                                  # apt deps, rustup if needed, release build, ~/.local/bin/clipbrain,
                                              # desktop file + icon, optional GNOME shortcut + autostart

# Headless UI test (drives the real window; ignored by default because it needs a display)
gtk4-broadwayd :9 &
GDK_BACKEND=broadway BROADWAY_DISPLAY=:9 cargo test -- --ignored
```

The app is single-instance (`app.clipbrain`): launching it while it is running just activates the running instance, which shows its window. To try a dev build next to an installed one, give it a private bus and data dir: `XDG_DATA_HOME=/tmp/cb dbus-run-session -- cargo run` (there's no tray in that case, since the private bus has no tray host).

Build needs `libgtk-4-dev` (GTK ≥ 4.22), `libadwaita-1-dev` (≥ 1.9), `pkg-config` and `glib-compile-resources` (pulled in by `libgtk-4-dev`). The `v4_22` / `v1_9` crate features in `Cargo.toml` are pinned to those versions.

## Architecture

One process. GTK lives on the main thread; the tray runs on ksni's own thread; file I/O and hashing run on GIO's blocking pool.

- `main.rs`: sets `GDK_BACKEND=x11` (see below), initialises `env_logger` (`CLIPBRAIN_LOG`), registers the GResource, and runs `ClipbrainApp`.
- `app.rs`: `ClipbrainApp` (`AdwApplication` subclass). `startup` holds the app and sets accelerators. The first `activate` opens the store off-thread, then creates the `Monitor`, the single `HistoryWindow` and the tray. Later activations call `show_window()`. Also provides `with_store()`, which runs a closure against the `Arc<Mutex<Store>>` in `gio::spawn_blocking`. Use it for every storage call from GTK code.
- `storage.rs`: `Store`, the in-memory copy of `history.json`, written atomically (temp file + fsync + rename) after each change. Has no GTK imports. Text dedups by exact content; images dedup by SHA-256 `content_hash`; re-adding moves the entry to the front and keeps its id and timestamp. Capped at `MAX_ENTRIES` (200); evicted PNGs are deleted. Directories are 0700, files 0600.
- `monitor.rs`: `ClipboardSource` trait (`GdkClipboardSource` in production, a scripted fake in tests) and `Monitor`, which reports each new distinct `Capture` (text, or PNG bytes).
- `entry.rs`: `ClipEntry` GObject. It holds the thumbnail cache; thumbnails are decoded and box-filtered to 48 px off-thread on first bind.
- `window/`: `HistoryWindow` (`window.ui` template): `ListStore<ClipEntry>` → `FilterListModel` (`CustomFilter`) → `SingleSelection` → `ListView` with `EntryRow` (`entry-row.ui`). Row buttons use the `win.copy-entry` / `win.delete-entry` actions with the entry id as target. Space opens `PreviewWindow` (`preview-window.ui`), which shows the full text (capped at 1 MB) or the full-size image. It's a separate modal `AdwWindow`, not an `AdwDialog`, because a dialog can't grow beyond the 480 px history window. `+`/`-` resize it by 10%, and its last size is reused.
- `tray.rs`: `ksni` StatusNotifierItem. Sends `TrayEvent`s over an `async_channel` to a `glib::spawn_future_local` loop in `app.rs`.
- `resources/`: `.ui` files, `icons/app.clipbrain.svg` (also bundled in the GResource), `app.clipbrain.desktop`. Compiled by `build.rs`.

## Behaviour that looks wrong but is intentional

- **The app runs under XWayland (`GDK_BACKEND=x11`, set in `main.rs`).** On native Wayland, mutter only delivers clipboard updates to the focused client, so copies made while the window is hidden are missed. X11 clients get every selection change. Don't remove this without replacing it with another background capture path.
- **The 500 ms poll in `monitor.rs` is required.** Mutter coalesces `gdk::Clipboard`'s `changed` signal, so rapid consecutive copies from one app fire only once. The poll catches the rest. Do not remove it.
- **`last_hash` dedup in the monitor.** GNOME fires spurious `changed` signals on focus changes; the monitor suppresses them by hashing content. It is seeded from `Store::seed_hash()` on startup so restarts don't re-store the current clipboard. After clearing history, `Monitor::reset_last_hash()` must run (wired via the window's `history-cleared` signal) or re-copying the same text is silently dropped.
- **Content marked secret by a password manager is never read or stored.** The monitor checks for `x-kde-passwordManagerHint` with the value `secret` (the KeePassXC/Klipper convention) and for `application/x-nspasteboard-concealed-type`. If the hint is present but can't be read, the content is treated as secret.
- **Our own clipboard writes are ignored** (`clipboard.is_local()`), so copying an image back doesn't re-capture a re-encoded duplicate. The copy action calls `Store::promote()` explicitly instead.
- **The window is never destroyed.** `HistoryWindow` is created once; `close_request` hides it. `show_window()` always calls `reload()` before `present()` so entries captured while hidden appear.
- **Live updates only when visible.** Captures call `window.reload()` only if the window is visible; otherwise entries accumulate in storage until the next show.
- **The capture-phase key handler yields to dialogs and to typing.** List keys are ignored while a dialog is open. The preview window sends its keys to `HistoryWindow::handle_preview_key` (Space/Escape, Enter, j/k, +/-). `j`, `k`, `?`, Space and `Delete` go to the search entry while it has focus.
- **Don't edit `history.json` while the app runs.** `Store` keeps the history in memory and is the only writer.
