"""
monitor.py — GTK 4 clipboard watcher.

Connects to the GDK display clipboard and fires callbacks whenever the
content changes.  Both plain-text and image payloads are handled.

Detection order: images are checked BEFORE text, because many apps that
copy an image also advertise text/plain (e.g. a file URI).  We treat any
clipboard that contains an image type as an image.

On GNOME/Wayland the `changed` signal is coalesced by mutter — when the
same app copies several items in quick succession only the last one fires
the signal — and it is only delivered while our window has keyboard focus.
We complement the signal with a 500 ms polling timer that reads the
clipboard and captures any hash change the signal missed; while the window
is hidden the poll is the only thing capturing copies.

Usage:
    monitor = ClipboardMonitor(on_text=..., on_image=...)
    monitor.start()   # attaches to the default display's clipboard
"""

from __future__ import annotations

import hashlib
from typing import Callable

import gi

gi.require_version("Gdk", "4.0")
gi.require_version("GdkPixbuf", "2.0")

from gi.repository import Gdk, GdkPixbuf, GLib, Gio

_IMAGE_MIMES = ("image/png", "image/jpeg", "image/bmp", "image/tiff", "image/webp")

# Polling interval in milliseconds.  500 ms is a good balance: fast enough
# to catch items copied within the same second, slow enough to not hammer
# the compositor.
_POLL_MS = 500

# A poll read that hasn't completed after this long is cancelled.  Without
# it, a source app that never finishes writing would stall polling forever.
_POLL_TIMEOUT_US = 3_000_000


class ClipboardMonitor:
    def __init__(
        self,
        on_text: Callable[[str], None],
        on_image: Callable[[bytes], None],
    ) -> None:
        self._on_text = on_text
        self._on_image = on_image
        self._clipboard: Gdk.Clipboard | None = None
        # Sequence counter: each changed event gets a generation number.
        # Async callbacks that belong to a superseded generation are discarded,
        # preventing stale reads from overwriting newer content — without ever
        # blocking a new changed event while a read is in flight.
        self._generation: int = 0
        # Cancellable for the current in-flight async read; cancelled and
        # replaced every time a new `changed` event arrives so the Wayland
        # compositor sees the previous offer released immediately.
        self._cancellable: Gio.Cancellable = Gio.Cancellable()
        # Hash of the last content delivered — prevents duplicate captures
        # when the clipboard `changed` signal fires on focus changes.
        self._last_hash: str | None = None
        # Whether a poll-triggered async read is currently in flight.
        # We skip the next poll tick if one is already pending to avoid
        # stacking up reads.  Every poll completion path must call
        # _poll_done(), otherwise polling stops for good.
        self._poll_reading: bool = False
        self._poll_started_us: int = 0
        self._poll_cancellable: Gio.Cancellable = Gio.Cancellable()
        # Hash of the raw image bytes last seen by the poll, so an image
        # sitting on the clipboard isn't decoded and re-encoded every tick.
        self._poll_raw_image_hash: str | None = None

    def start(self) -> None:
        display = Gdk.Display.get_default()
        if display is None:
            return
        self._clipboard = display.get_clipboard()
        self._seed_from_current()
        self._clipboard.connect("changed", self._on_changed)
        GLib.timeout_add(_POLL_MS, self._poll)

    def _seed_from_current(self) -> None:
        """Seed _last_hash from storage so restarts don't re-store existing content."""
        from clipbrain import storage
        entries = storage.load()
        if not entries:
            return
        top = entries[0]
        if top.get("kind") == "text" and top.get("text"):
            self._last_hash = hashlib.sha256(top["text"].encode()).hexdigest()
        elif top.get("kind") == "image" and top.get("content_hash"):
            self._last_hash = top["content_hash"]

    def reset_last_hash(self) -> None:
        """Clear the dedup hash — call this when history is cleared so the
        current clipboard content can be re-captured if copied again."""
        self._last_hash = None

    def _poll(self) -> bool:
        """GLib timer callback — reads clipboard and stores any change missed
        by the `changed` signal.  Returns True to keep the timer.

        On GNOME/Wayland only the focused client receives selection updates,
        so while the window is hidden this poll is the only capture path.
        It does not bump _generation (that would discard in-flight `changed`
        reads); instead it drops its own result if a `changed` event arrived
        while it was reading."""
        if self._clipboard is None:
            return True
        if self._poll_reading:
            if GLib.get_monotonic_time() - self._poll_started_us < _POLL_TIMEOUT_US:
                return True
            self._poll_cancellable.cancel()  # stuck read; its callback is ignored
            self._poll_done()
        formats = self._clipboard.get_formats()
        if formats is None:
            return True
        image_mime = self._find_image_mime(formats)
        has_text = self._has_text(formats)
        if not image_mime and not has_text:
            return True
        self._poll_reading = True
        self._poll_started_us = GLib.get_monotonic_time()
        self._poll_cancellable = Gio.Cancellable()
        cancellable = self._poll_cancellable
        generation = self._generation
        if image_mime:
            self._clipboard.read_async(
                [image_mime], GLib.PRIORITY_DEFAULT, cancellable,
                self._make_poll_image_callback(generation, cancellable, has_text))
        else:
            self._poll_read_text(generation, cancellable)
        return True  # keep timer running

    def _poll_done(self) -> None:
        self._poll_reading = False

    def _poll_read_text(self, generation: int, cancellable: Gio.Cancellable) -> None:
        def _on_text_ready(clipboard: Gdk.Clipboard, result: Gio.AsyncResult) -> None:
            if cancellable is not self._poll_cancellable:
                return  # timed out and superseded by a newer poll
            self._poll_done()
            try:
                text = clipboard.read_text_finish(result)
            except Exception:
                return
            if generation != self._generation:
                return  # a `changed` read owns this content now
            self._deliver_text(text)
        self._clipboard.read_text_async(cancellable, _on_text_ready)

    def _make_poll_image_callback(self, generation: int, cancellable: Gio.Cancellable,
                                  has_text: bool):
        def _fallback_to_text() -> None:
            # The format list may be stale (it only refreshes while focused),
            # so the current content may be text even though an image was
            # advertised last time.
            if has_text:
                self._poll_read_text(generation, cancellable)
            else:
                self._poll_done()

        def _on_bytes(raw: bytes | None) -> None:
            if cancellable is not self._poll_cancellable:
                return
            if not raw:
                _fallback_to_text()
                return
            self._poll_done()
            if generation != self._generation:
                return
            raw_hash = hashlib.sha256(raw).hexdigest()
            if raw_hash == self._poll_raw_image_hash:
                return
            self._poll_raw_image_hash = raw_hash
            self._on_image_bytes_ready(raw, generation)

        def _on_stream_ready(clipboard: Gdk.Clipboard, result: Gio.AsyncResult) -> None:
            if cancellable is not self._poll_cancellable:
                return
            try:
                stream, _out_mime = clipboard.read_finish(result)
            except Exception:
                _fallback_to_text()
                return
            _read_stream_fully(stream, _on_bytes)
        return _on_stream_ready

    def _on_changed(self, clipboard: Gdk.Clipboard) -> None:
        # Cancel any in-flight async read so the Wayland compositor releases
        # the previous clipboard offer immediately — otherwise it withholds
        # subsequent `changed` signals until the read completes.
        self._cancellable.cancel()
        self._cancellable = Gio.Cancellable()
        self._poll_cancellable.cancel()

        # Bump generation — stale callbacks discard their result.
        self._generation += 1
        generation = self._generation
        cancellable = self._cancellable

        formats: Gdk.ContentFormats = clipboard.get_formats()
        image_mime = self._find_image_mime(formats)

        if image_mime is not None:
            clipboard.read_texture_async(
                cancellable, self._make_texture_callback(generation))
        else:
            clipboard.read_text_async(
                cancellable, self._make_text_callback(generation))

    @staticmethod
    def _has_text(formats: Gdk.ContentFormats) -> bool:
        return (formats.contain_mime_type("text/plain;charset=utf-8") or
                formats.contain_mime_type("text/plain"))

    def _deliver_text(self, text: str | None) -> None:
        if not text or not text.strip():
            return
        content_hash = hashlib.sha256(text.encode()).hexdigest()
        if content_hash == self._last_hash:
            return
        self._last_hash = content_hash
        self._on_text(text)

    @staticmethod
    def _find_image_mime(formats: Gdk.ContentFormats) -> str | None:
        for mime in _IMAGE_MIMES:
            if formats.contain_mime_type(mime):
                return mime
        return None

    # ------------------------------------------------------------------
    # Per-generation callback factories
    # ------------------------------------------------------------------

    def _make_text_callback(self, generation: int):
        def _on_text_ready(clipboard: Gdk.Clipboard, result: Gio.AsyncResult) -> None:
            if generation != self._generation:
                return  # superseded by a newer changed event
            try:
                text = clipboard.read_text_finish(result)
            except Exception:
                return
            self._deliver_text(text)
        return _on_text_ready

    def _make_texture_callback(self, generation: int):
        def _on_texture_ready(clipboard: Gdk.Clipboard, result: Gio.AsyncResult) -> None:
            if generation != self._generation:
                return
            try:
                texture = clipboard.read_texture_finish(result)
            except Exception:
                texture = None

            if texture is None:
                # Fallback: raw mime stream
                formats = clipboard.get_formats()
                mime = self._find_image_mime(formats)
                if mime:
                    clipboard.read_async(
                        [mime], GLib.PRIORITY_DEFAULT, None,
                        self._make_image_stream_callback(generation), mime)
                return

            try:
                pixbuf = Gdk.pixbuf_get_from_texture(texture)
                if pixbuf is None:
                    return
                success, png_bytes = pixbuf.save_to_bufferv("png", [], [])
                if success and png_bytes:
                    self._deliver_image(bytes(png_bytes), generation)
            except Exception:
                pass
        return _on_texture_ready

    def _make_image_stream_callback(self, generation: int):
        def _on_stream_ready(clipboard: Gdk.Clipboard, result: Gio.AsyncResult, mime: str) -> None:
            if generation != self._generation:
                return
            try:
                stream, _out_mime = clipboard.read_finish(result)
            except Exception:
                return
            _read_stream_fully(stream, lambda raw: self._on_image_bytes_ready(raw, generation))
        return _on_stream_ready

    def _on_image_bytes_ready(self, raw: bytes | None, generation: int) -> None:
        if not raw or generation != self._generation:
            return
        try:
            loader = GdkPixbuf.PixbufLoader()
            loader.write(raw)
            loader.close()
            pixbuf = loader.get_pixbuf()
            if pixbuf is None:
                return
            success, png_bytes = pixbuf.save_to_bufferv("png", [], [])
            if success and png_bytes:
                self._deliver_image(bytes(png_bytes), generation)
        except Exception:
            pass

    def _deliver_image(self, png_bytes: bytes, generation: int) -> None:
        if generation != self._generation:
            return
        content_hash = hashlib.sha256(png_bytes).hexdigest()
        if content_hash == self._last_hash:
            return
        self._last_hash = content_hash
        self._on_image(png_bytes)


# ---------------------------------------------------------------------------
# Helper: fully read a Gio.InputStream asynchronously
# ---------------------------------------------------------------------------

_CHUNK = 65536


def _read_stream_fully(
    stream: Gio.InputStream,
    callback: Callable[[bytes | None], None],
) -> None:
    buf = bytearray()

    def _read_chunk(stream: Gio.InputStream, result: Gio.AsyncResult) -> None:
        try:
            chunk = stream.read_bytes_finish(result)
        except Exception:
            callback(None)
            return
        data = chunk.get_data()
        if data:
            buf.extend(data)
            stream.read_bytes_async(_CHUNK, GLib.PRIORITY_DEFAULT, None, _read_chunk)
        else:
            stream.close_async(GLib.PRIORITY_DEFAULT, None, lambda *_: None)
            callback(bytes(buf))

    stream.read_bytes_async(_CHUNK, GLib.PRIORITY_DEFAULT, None, _read_chunk)
