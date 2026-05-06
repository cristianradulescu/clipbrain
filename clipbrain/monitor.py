"""
monitor.py — GTK 4 clipboard watcher.

Connects to the GDK display clipboard and fires callbacks whenever the
content changes.  Both plain-text and image payloads are handled.

Detection order: images are checked BEFORE text, because many apps that
copy an image also advertise text/plain (e.g. a file URI).  We treat any
clipboard that contains an image type as an image.

On GNOME/Wayland the `changed` signal is coalesced by mutter — when the
same app copies several items in quick succession only the last one fires
the signal.  We complement the signal with a 500 ms polling timer that
reads the clipboard and captures any hash change the signal missed.

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
        # stacking up reads.
        self._poll_reading: bool = False

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
        by the coalesced `changed` signal.  Returns True to keep the timer."""
        if self._clipboard is None or self._poll_reading:
            return True
        formats = self._clipboard.get_formats()
        if formats is None:
            return True
        image_mime = self._find_image_mime(formats)
        has_text = (formats.contain_mime_type("text/plain;charset=utf-8") or
                    formats.contain_mime_type("text/plain"))
        if not image_mime and not has_text:
            return True
        self._poll_reading = True
        self._generation += 1
        generation = self._generation
        if image_mime:
            self._clipboard.read_texture_async(
                self._cancellable, self._make_texture_callback(generation))
        else:
            self._clipboard.read_text_async(
                self._cancellable, self._make_poll_text_callback(generation))
        return True  # keep timer running

    def _make_poll_text_callback(self, generation: int):
        def _on_text_ready(clipboard: Gdk.Clipboard, result: Gio.AsyncResult) -> None:
            self._poll_reading = False
            try:
                text = clipboard.read_text_finish(result)
            except Exception:
                return
            if not text or not text.strip():
                return
            content_hash = hashlib.sha256(text.encode()).hexdigest()
            if content_hash == self._last_hash:
                return
            self._last_hash = content_hash
            self._on_text(text)
        return _on_text_ready

    def _on_changed(self, clipboard: Gdk.Clipboard) -> None:
        # Cancel any in-flight async read so the Wayland compositor releases
        # the previous clipboard offer immediately — otherwise it withholds
        # subsequent `changed` signals until the read completes.
        self._cancellable.cancel()
        self._cancellable = Gio.Cancellable()

        # Bump generation — stale callbacks discard their result.
        self._generation += 1
        generation = self._generation
        cancellable = self._cancellable

        formats: Gdk.ContentFormats = clipboard.get_formats()
        image_mime = self._find_image_mime(formats)
        has_text = formats.contain_mime_type("text/plain;charset=utf-8") or \
                   formats.contain_mime_type("text/plain")

        if image_mime is not None:
            clipboard.read_texture_async(
                cancellable, self._make_texture_callback(generation))
        elif formats.contain_mime_type("text/plain;charset=utf-8") or \
             formats.contain_mime_type("text/plain"):
            clipboard.read_text_async(
                cancellable, self._make_text_callback(generation))
        else:
            clipboard.read_text_async(
                cancellable, self._make_text_callback(generation))

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
            if text and text.strip():
                content_hash = hashlib.sha256(text.encode()).hexdigest()
                if content_hash == self._last_hash:
                    return
                self._last_hash = content_hash
                self._on_text(text)
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
