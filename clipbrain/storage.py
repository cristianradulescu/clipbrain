"""
storage.py — Persistent clipboard history.

Each entry is stored as a dict in a JSON index file:
  {
    "id": "<uuid>",
    "kind": "text" | "image",
    "timestamp": "<ISO-8601>",
    "text": "...",          # kind == "text"
    "image_path": "...",    # kind == "image", absolute path under data dir
  }

Images are saved as PNG files named by their entry id.
The most-recent entry is at index 0 (prepend on add).
A maximum of MAX_ENTRIES entries are kept; oldest are pruned automatically.
"""

from __future__ import annotations

import json
import os
import shutil
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import TypedDict

MAX_ENTRIES = 200

_DATA_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "clipbrain"
_INDEX_FILE = _DATA_DIR / "history.json"
_IMAGES_DIR = _DATA_DIR / "images"


class Entry(TypedDict, total=False):
    id: str
    kind: str          # "text" | "image"
    timestamp: str
    text: str          # present when kind == "text"
    image_path: str    # present when kind == "image"


def _ensure_dirs() -> None:
    _DATA_DIR.mkdir(parents=True, exist_ok=True)
    _IMAGES_DIR.mkdir(parents=True, exist_ok=True)


def load() -> list[Entry]:
    """Return the full history list (most-recent first)."""
    _ensure_dirs()
    if not _INDEX_FILE.exists():
        return []
    try:
        with _INDEX_FILE.open("r", encoding="utf-8") as fh:
            return json.load(fh)
    except (json.JSONDecodeError, OSError):
        return []


def _save(entries: list[Entry]) -> None:
    _ensure_dirs()
    with _INDEX_FILE.open("w", encoding="utf-8") as fh:
        json.dump(entries, fh, ensure_ascii=False, indent=2)


def add_text(text: str) -> Entry:
    """Prepend a text entry.  Returns the new entry.

    If the same text already exists, that entry is moved to the front and
    its original id/timestamp are preserved so the window can remove the
    old row without a full reload.
    """
    entries = load()

    # De-duplicate: reuse the existing entry if the text is identical.
    existing: Entry | None = None
    for e in entries:
        if e.get("kind") == "text" and e.get("text") == text:
            existing = e
            break

    if existing is not None:
        entries = [e for e in entries if e["id"] != existing["id"]]
        entries.insert(0, existing)
        _save(entries)
        return existing

    entry: Entry = {
        "id": str(uuid.uuid4()),
        "kind": "text",
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "text": text,
    }
    entries.insert(0, entry)
    _prune(entries)
    _save(entries)
    return entry


def add_image(png_bytes: bytes) -> Entry:
    """Save PNG bytes to disk and prepend an image entry.  Returns the new entry.

    If an identical image (same SHA-256) already exists, that entry is moved
    to the front and its original id/path are preserved.
    """
    import hashlib
    content_hash = hashlib.sha256(png_bytes).hexdigest()

    entries = load()

    # De-duplicate by hash
    existing: Entry | None = None
    for e in entries:
        if e.get("kind") == "image" and e.get("content_hash") == content_hash:
            existing = e
            break

    if existing is not None:
        entries = [e for e in entries if e["id"] != existing["id"]]
        entries.insert(0, existing)
        _save(entries)
        return existing

    _ensure_dirs()
    entry_id = str(uuid.uuid4())
    image_path = str(_IMAGES_DIR / f"{entry_id}.png")
    with open(image_path, "wb") as fh:
        fh.write(png_bytes)

    entry: Entry = {
        "id": entry_id,
        "kind": "image",
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "image_path": image_path,
        "content_hash": content_hash,
    }
    entries.insert(0, entry)
    _prune(entries)
    _save(entries)
    return entry


def delete(entry_id: str) -> None:
    """Remove an entry by id, deleting its image file if applicable."""
    entries = load()
    new_entries = []
    for e in entries:
        if e["id"] == entry_id:
            if e.get("kind") == "image" and e.get("image_path"):
                try:
                    os.unlink(e["image_path"])
                except OSError:
                    pass
        else:
            new_entries.append(e)
    _save(new_entries)


def clear_all() -> None:
    """Delete all entries and image files."""
    if _IMAGES_DIR.exists():
        shutil.rmtree(_IMAGES_DIR)
    _IMAGES_DIR.mkdir(parents=True, exist_ok=True)
    _save([])


def _prune(entries: list[Entry]) -> None:
    """Trim to MAX_ENTRIES in-place, deleting orphaned image files."""
    while len(entries) > MAX_ENTRIES:
        oldest = entries.pop()
        if oldest.get("kind") == "image" and oldest.get("image_path"):
            try:
                os.unlink(oldest["image_path"])
            except OSError:
                pass
