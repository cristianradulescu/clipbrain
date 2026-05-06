"""
window.py — Main clipboard history window (GTK 4 + libadwaita).

Shows all stored entries in a scrollable list.  Each row displays:
  - Text entries: icon + first line of text (truncated)
  - Image entries: thumbnail + dimensions label

Actions per row:
  - Click "Copy" button  → put the entry back on the clipboard
  - Click "Delete" icon  → remove the entry from history

The window hides (rather than closes) when the user presses the X button,
so the app keeps running in the background.
"""

from __future__ import annotations

import os
from typing import Callable

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")
gi.require_version("GdkPixbuf", "2.0")

from gi.repository import Adw, Gdk, GdkPixbuf, GLib, Gio, GObject, Gtk

from clipbrain import storage

THUMBNAIL_SIZE = 48   # px — square thumbnail for image entries
TEXT_MAX_CHARS = 120  # chars shown in list row before truncation


class HistoryWindow(Adw.ApplicationWindow):
    def __init__(self, on_history_cleared: Callable = None, **kwargs):
        super().__init__(**kwargs)
        self._on_history_cleared = on_history_cleared
        self.set_title("Clipbrain - Clipboard History")
        self.set_default_size(480, 640)

        # Closing the window just hides it so the daemon keeps running
        self.connect("close-request", self._on_close_request)

        # ── Layout ──────────────────────────────────────────────────────
        toolbar_view = Adw.ToolbarView()
        self.set_content(toolbar_view)

        header = Adw.HeaderBar()
        toolbar_view.add_top_bar(header)

        # Clear-all button
        clear_btn = Gtk.Button(icon_name="edit-clear-all-symbolic")
        clear_btn.set_tooltip_text("Clear all history")
        clear_btn.connect("clicked", self._on_clear_all)
        header.pack_end(clear_btn)

        # Help button
        help_btn = Gtk.Button(label="?")
        help_btn.set_tooltip_text("Keyboard shortcuts (? or F1)")
        help_btn.connect("clicked", self._on_shortcuts_clicked)
        header.pack_end(help_btn)

        # Search toggle
        self._search_btn = Gtk.ToggleButton(icon_name="system-search-symbolic")
        self._search_btn.set_tooltip_text("Search (Ctrl+F)")
        self._search_btn.connect("toggled", self._on_search_toggled)
        header.pack_end(self._search_btn)

        # ── Search bar ──────────────────────────────────────────────────
        self._search_bar = Gtk.SearchBar()
        self._search_bar.set_show_close_button(True)
        self._search_entry = Gtk.SearchEntry()
        self._search_entry.set_hexpand(True)
        self._search_entry.connect("search-changed", self._on_search_changed)
        self._search_bar.set_child(self._search_entry)
        self._search_bar.connect_entry(self._search_entry)
        self._search_bar.bind_property(
            "search-mode-enabled", self._search_btn, "active",
            GObject.BindingFlags.BIDIRECTIONAL,
        )

        # ── List model ──────────────────────────────────────────────────
        self._store = Gio.ListStore(item_type=EntryObject)
        self._filter_model = Gtk.FilterListModel(model=self._store)
        self._filter = Gtk.CustomFilter()
        self._filter.set_filter_func(self._filter_func)
        self._filter_model.set_filter(self._filter)

        self._list_view_factory = Gtk.SignalListItemFactory()
        self._list_view_factory.connect("setup", self._on_item_setup)
        self._list_view_factory.connect("bind", self._on_item_bind)
        self._list_view_factory.connect("unbind", self._on_item_unbind)

        self._selection_model = Gtk.SingleSelection(model=self._filter_model)
        self._selection_model.set_autoselect(False)
        self._selection_model.set_can_unselect(True)
        self._list_view = Gtk.ListView(
            model=self._selection_model,
            factory=self._list_view_factory,
        )
        self._list_view.set_single_click_activate(False)
        self._list_view.add_css_class("clipboard-list")

        scroll = Gtk.ScrolledWindow()
        scroll.set_vexpand(True)
        scroll.set_hexpand(True)
        scroll.set_child(self._list_view)

        # ── Empty-state page ────────────────────────────────────────────
        self._status_page = Adw.StatusPage()
        self._status_page.set_title("No clipboard history yet")
        self._status_page.set_description("Copy some text or an image to get started")
        self._status_page.set_icon_name("edit-paste-symbolic")

        # ── Stack: list vs empty state ───────────────────────────────────
        self._stack = Gtk.Stack()
        self._stack.add_named(self._status_page, "empty")
        self._stack.add_named(scroll, "list")

        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        outer.append(self._search_bar)
        outer.append(self._stack)
        toolbar_view.set_content(outer)

        # ── Keyboard shortcuts ──────────────────────────────────────────
        key_ctrl = Gtk.EventControllerKey()
        key_ctrl.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        key_ctrl.connect("key-pressed", self._on_key_pressed)
        self.add_controller(key_ctrl)

        shortcuts_action = Gio.SimpleAction.new("shortcuts", None)
        shortcuts_action.connect("activate", self._on_shortcuts_clicked)
        self.add_action(shortcuts_action)
        app = self.get_application()
        if app:
            app.set_accels_for_action("win.shortcuts", ["question", "F1"])

        # Initial load
        self.reload()

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def reload(self) -> None:
        """Reload from storage and refresh the list."""
        self._store.remove_all()
        entries = storage.load()
        for e in entries:
            self._store.append(EntryObject(e))
        self._update_empty_state()

    def prepend_entry(self, entry: storage.Entry) -> None:
        """Insert entry at the top, removing any existing row with the same id.

        storage.add_text reuses the existing entry object (same id) when
        deduplicating, so we must evict the old row before prepending.
        """
        entry_id = entry["id"]
        for i in range(self._store.get_n_items()):
            obj = self._store.get_item(i)
            if obj and obj.entry["id"] == entry_id:
                self._store.remove(i)
                break
        self._store.insert(0, EntryObject(entry))
        self._update_empty_state()

    # ------------------------------------------------------------------
    # List factory callbacks
    # ------------------------------------------------------------------

    def _on_item_setup(self, _factory, list_item: Gtk.ListItem) -> None:
        row = _EntryRow()
        list_item.set_child(row)

    def _on_item_bind(self, _factory, list_item: Gtk.ListItem) -> None:
        row: _EntryRow = list_item.get_child()
        obj: EntryObject = list_item.get_item()
        row.bind(obj.entry, self._copy_entry, self._delete_entry)

    def _on_item_unbind(self, _factory, list_item: Gtk.ListItem) -> None:
        row: _EntryRow = list_item.get_child()
        row.unbind()

    # ------------------------------------------------------------------
    # Entry actions
    # ------------------------------------------------------------------

    def _copy_entry(self, entry: storage.Entry) -> None:
        clipboard = self.get_clipboard()
        if entry.get("kind") == "text":
            text = entry.get("text", "")
            provider = Gdk.ContentProvider.new_for_value(text)
            clipboard.set_content(provider)
        elif entry.get("kind") == "image":
            path = entry.get("image_path", "")
            if path and os.path.exists(path):
                try:
                    pixbuf = GdkPixbuf.Pixbuf.new_from_file(path)
                    provider = Gdk.ContentProvider.new_for_value(pixbuf)
                    clipboard.set_content(provider)
                except Exception:
                    pass

    def _delete_entry(self, entry: storage.Entry) -> None:
        entry_id = entry["id"]
        storage.delete(entry_id)
        # Remove from store
        for i in range(self._store.get_n_items()):
            obj = self._store.get_item(i)
            if obj and obj.entry["id"] == entry_id:
                self._store.remove(i)
                break
        self._update_empty_state()

    # ------------------------------------------------------------------
    # Clear all
    # ------------------------------------------------------------------

    def _on_clear_all(self, _btn) -> None:
        dialog = Adw.AlertDialog()
        dialog.set_heading("Clear all history?")
        dialog.set_body("All clipboard entries will be permanently deleted.")
        dialog.add_response("cancel", "Cancel")
        dialog.add_response("clear", "Clear All")
        dialog.set_response_appearance("clear", Adw.ResponseAppearance.DESTRUCTIVE)
        dialog.set_default_response("cancel")
        dialog.set_close_response("cancel")
        dialog.connect("response", self._on_clear_confirmed)
        dialog.present(self)

    def _on_clear_confirmed(self, dialog, response_id: str) -> None:
        if response_id == "clear":
            storage.clear_all()
            self._store.remove_all()
            self._update_empty_state()
            if self._on_history_cleared:
                self._on_history_cleared()

    # ------------------------------------------------------------------
    # Search
    # ------------------------------------------------------------------

    def _on_search_toggled(self, btn) -> None:
        self._search_bar.set_search_mode(btn.get_active())

    def _on_search_changed(self, entry) -> None:
        self._filter.changed(Gtk.FilterChange.DIFFERENT)

    def _filter_func(self, obj: EntryObject) -> bool:
        query = self._search_entry.get_text().lower().strip()
        if not query:
            return True
        entry = obj.entry
        if entry.get("kind") == "text":
            return query in (entry.get("text") or "").lower()
        # Images: match by date/kind keyword
        return query in "image"

    # ------------------------------------------------------------------
    # Keyboard
    # ------------------------------------------------------------------

    def _on_key_pressed(self, ctrl, keyval, _keycode, state) -> bool:
        ctrl_mask = Gdk.ModifierType.CONTROL_MASK
        shift_mask = Gdk.ModifierType.SHIFT_MASK

        # Ctrl+F — toggle search
        if keyval == Gdk.KEY_f and (state & ctrl_mask):
            self._search_btn.set_active(not self._search_btn.get_active())
            return True

        # "/" — open search (unless already typing somewhere)
        if keyval == Gdk.KEY_slash and not self._search_bar.get_search_mode():
            self._search_btn.set_active(True)
            return True

        # Escape — close search or hide window
        if keyval == Gdk.KEY_Escape:
            if self._search_bar.get_search_mode():
                self._search_btn.set_active(False)
                return True
            self.set_visible(False)
            return True

        # Ctrl+Shift+Delete — clear all history
        if keyval == Gdk.KEY_Delete and (state & ctrl_mask) and (state & shift_mask):
            self._on_clear_all(None)
            return True

        # Arrow down / j — select next item
        if keyval in (Gdk.KEY_Down, Gdk.KEY_j):
            self._move_selection(1)
            return True

        # Arrow up / k — select previous item
        if keyval in (Gdk.KEY_Up, Gdk.KEY_k):
            self._move_selection(-1)
            return True

        # Enter — copy selected entry
        if keyval in (Gdk.KEY_Return, Gdk.KEY_KP_Enter):
            entry = self._selected_entry()
            if entry is not None:
                self._copy_entry(entry)
            return True

        # Delete — delete selected entry
        if keyval == Gdk.KEY_Delete:
            entry = self._selected_entry()
            if entry is not None:
                self._delete_entry(entry)
            return True

        return False

    def _selected_entry(self) -> storage.Entry | None:
        pos = self._selection_model.get_selected()
        if pos == Gtk.INVALID_LIST_POSITION:
            return None
        obj = self._filter_model.get_item(pos)
        return obj.entry if obj else None

    def _move_selection(self, delta: int) -> None:
        n = self._filter_model.get_n_items()
        if n == 0:
            return
        current = self._selection_model.get_selected()
        if current == Gtk.INVALID_LIST_POSITION:
            new_pos = 0 if delta > 0 else n - 1
        else:
            new_pos = max(0, min(n - 1, current + delta))
        self._selection_model.set_selected(new_pos)
        self._list_view.scroll_to(new_pos, Gtk.ListScrollFlags.NONE, None)

    # ------------------------------------------------------------------
    # Shortcuts window
    # ------------------------------------------------------------------

    def _on_shortcuts_clicked(self, *_) -> None:
        win = self._build_shortcuts_window()
        win.set_transient_for(self)
        win.present()

    def _build_shortcuts_window(self) -> Gtk.ShortcutsWindow:
        nav_group = Gtk.ShortcutsGroup(title="Navigation")
        nav_group.append(Gtk.ShortcutsShortcut(title="Select next item",     accelerator="Down j"))
        nav_group.append(Gtk.ShortcutsShortcut(title="Select previous item", accelerator="Up k"))

        action_group = Gtk.ShortcutsGroup(title="Actions")
        action_group.append(Gtk.ShortcutsShortcut(title="Copy selected item",   accelerator="Return"))
        action_group.append(Gtk.ShortcutsShortcut(title="Delete selected item", accelerator="Delete"))
        action_group.append(Gtk.ShortcutsShortcut(title="Clear all history",    accelerator="<Control><Shift>Delete"))

        search_group = Gtk.ShortcutsGroup(title="Search")
        search_group.append(Gtk.ShortcutsShortcut(title="Search",        accelerator="<Control>f slash"))
        search_group.append(Gtk.ShortcutsShortcut(title="Close search",  accelerator="Escape"))

        misc_group = Gtk.ShortcutsGroup(title="General")
        misc_group.append(Gtk.ShortcutsShortcut(title="Show this window", accelerator="question F1"))

        section = Gtk.ShortcutsSection()
        section.append(nav_group)
        section.append(action_group)
        section.append(search_group)
        section.append(misc_group)

        window = Gtk.ShortcutsWindow()
        window.set_child(section)
        return window

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _update_empty_state(self) -> None:
        if self._store.get_n_items() == 0:
            self._stack.set_visible_child_name("empty")
        else:
            self._stack.set_visible_child_name("list")

    def _on_close_request(self, _win) -> bool:
        # Hide instead of destroy so the background daemon keeps running
        self.set_visible(False)
        return True  # prevent default destroy


# ---------------------------------------------------------------------------
# GObject wrapper so Gio.ListStore can hold Entry dicts
# ---------------------------------------------------------------------------

class EntryObject(GObject.Object):
    __gtype_name__ = "ClipboardEntryObject"

    def __init__(self, entry: storage.Entry):
        super().__init__()
        self.entry = entry


# ---------------------------------------------------------------------------
# Row widget
# ---------------------------------------------------------------------------

class _EntryRow(Gtk.Box):
    """A single row in the history list."""

    def __init__(self):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=0)

        # Inner horizontal box with the row content
        inner = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        inner.set_margin_top(8)
        inner.set_margin_bottom(8)
        inner.set_margin_start(12)
        inner.set_margin_end(8)
        self.append(inner)

        # Thumbnail (shown only for image entries)
        self._thumb_image = Gtk.Image()
        self._thumb_image.set_pixel_size(THUMBNAIL_SIZE)
        self._thumb_image.set_size_request(THUMBNAIL_SIZE, THUMBNAIL_SIZE)
        inner.append(self._thumb_image)

        # Text label
        self._label = Gtk.Label()
        self._label.set_hexpand(True)
        self._label.set_halign(Gtk.Align.START)
        self._label.set_valign(Gtk.Align.CENTER)
        self._label.set_ellipsize(3)  # PANGO_ELLIPSIZE_END
        self._label.set_max_width_chars(50)
        self._label.set_wrap(False)
        inner.append(self._label)

        # Copy button
        self._copy_btn = Gtk.Button(label="Copy")
        self._copy_btn.set_valign(Gtk.Align.CENTER)
        self._copy_btn.add_css_class("pill")
        inner.append(self._copy_btn)

        # Delete button
        self._del_btn = Gtk.Button(icon_name="edit-delete-symbolic")
        self._del_btn.set_valign(Gtk.Align.CENTER)
        self._del_btn.add_css_class("destructive-action")
        self._del_btn.add_css_class("flat")
        inner.append(self._del_btn)

        # Separator at the bottom of each row
        self._separator = Gtk.Separator(orientation=Gtk.Orientation.HORIZONTAL)
        self.append(self._separator)

        self._copy_handler: int | None = None
        self._del_handler: int | None = None
        self._entry: storage.Entry | None = None

    def bind(
        self,
        entry: storage.Entry,
        on_copy: Callable,
        on_delete: Callable,
    ) -> None:
        self._entry = entry

        if entry.get("kind") == "text":
            self._thumb_image.set_visible(False)
            text = entry.get("text", "")
            first_line = text.split("\n")[0]
            display = first_line[:TEXT_MAX_CHARS] + ("…" if len(first_line) > TEXT_MAX_CHARS else "")
            self._label.set_text(display)
            self._label.set_tooltip_text(text[:500])
        elif entry.get("kind") == "image":
            self._thumb_image.set_visible(True)
            path = entry.get("image_path", "")
            self._label.set_text("Image")
            if path and os.path.exists(path):
                try:
                    pixbuf = GdkPixbuf.Pixbuf.new_from_file_at_size(path, THUMBNAIL_SIZE, THUMBNAIL_SIZE)
                    self._thumb_image.set_from_pixbuf(pixbuf)
                    full = GdkPixbuf.Pixbuf.new_from_file(path)
                    self._label.set_text(f"Image  {full.get_width()} × {full.get_height()} px")
                except Exception:
                    self._thumb_image.set_from_icon_name("image-x-generic-symbolic")
            else:
                self._thumb_image.set_from_icon_name("image-missing-symbolic")

        self._copy_handler = self._copy_btn.connect("clicked", lambda _: on_copy(entry))
        self._del_handler = self._del_btn.connect("clicked", lambda _: on_delete(entry))

    def unbind(self) -> None:
        if self._copy_handler is not None:
            self._copy_btn.disconnect(self._copy_handler)
            self._copy_handler = None
        if self._del_handler is not None:
            self._del_btn.disconnect(self._del_handler)
            self._del_handler = None
        self._entry = None
        self._label.set_text("")
        self._thumb_image.clear()
        self._thumb_image.set_visible(True)
