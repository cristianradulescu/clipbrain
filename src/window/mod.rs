//! `HistoryWindow`: the clipboard history list with search and keyboard
//! navigation. Created once; closing hides it.

mod imp;
mod preview;
mod row;

use std::path::PathBuf;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use crate::app::{SharedStore, with_store};
use crate::entry::{ClipEntry, load_texture};
use crate::storage::Entry;
use preview::PreviewDialog;
use row::EntryRow;

glib::wrapper! {
    /// The clipboard history window.
    pub struct HistoryWindow(ObjectSubclass<imp::HistoryWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl HistoryWindow {
    /// Creates the window over `store`. Set its application separately.
    pub fn new(store: SharedStore) -> Self {
        let window: Self = glib::Object::new();
        let _ = window.imp().store.set(store);
        window
    }

    /// Re-reads the history from storage (off-thread) and refreshes the
    /// list. The selection follows its entry: `SingleSelection` keeps the
    /// selected item when a splice removes and re-adds it.
    pub fn reload(&self) {
        self.load(None);
    }

    /// Like [`reload`](Self::reload), but afterwards selects and scrolls to
    /// the entry with `select_id` if given.
    fn load(&self, select_id: Option<String>) {
        let Some(store) = self.imp().store.get().cloned() else {
            return;
        };
        let seq = self.bump_change_seq();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                match with_store(&store, |s| Ok(s.entries().to_vec())).await {
                    Ok(entries) if win.imp().change_seq.get() == seq => {
                        win.set_entries(&entries);
                        if let Some(id) = select_id {
                            win.select_entry(&id);
                        }
                    }
                    Ok(_) => log::debug!("dropping superseded reload"),
                    Err(e) => log::warn!("could not reload history: {e:#}"),
                }
            }
        ));
    }

    /// Calls `f` after the user clears all history.
    pub fn connect_history_cleared<F: Fn() + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "history-cleared",
            false,
            glib::closure_local!(move |_: &Self| f()),
        )
    }

    fn entries(&self) -> &gio::ListStore {
        &self.imp().entries
    }

    fn selection(&self) -> &gtk::SingleSelection {
        &self.imp().selection
    }

    fn bump_change_seq(&self) -> u64 {
        let seq = self.imp().change_seq.get() + 1;
        self.imp().change_seq.set(seq);
        seq
    }

    fn setup_list(&self) {
        let imp = self.imp();
        let search_entry = imp.search_entry.downgrade();
        imp.filter.set_filter_func(move |item| {
            let query = search_entry.upgrade().map(|e| e.text()).unwrap_or_default();
            item.downcast_ref::<ClipEntry>()
                .is_some_and(|entry| entry.matches(&query))
        });
        let filter = imp.filter.clone();
        imp.search_entry
            .connect_search_changed(move |_| filter.changed(gtk::FilterChange::Different));

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&EntryRow::new()));
            }
        });
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let row = item.child().and_downcast::<EntryRow>();
            let entry = item.item().and_downcast::<ClipEntry>();
            if let (Some(row), Some(entry)) = (row, entry) {
                row.bind(&entry);
            }
        });
        factory.connect_unbind(|_, item| {
            let row = item
                .downcast_ref::<gtk::ListItem>()
                .and_then(|item| item.child().and_downcast::<EntryRow>());
            if let Some(row) = row {
                row.unbind();
            }
        });

        imp.list_view.set_model(Some(&imp.selection));
        imp.list_view.set_factory(Some(&factory));

        imp.entries.connect_items_changed(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _, _, _| win.update_empty_state()
        ));
        self.update_empty_state();
    }

    /// Replaces the list contents, reusing existing `ClipEntry` objects so
    /// cached thumbnails survive reloads.
    fn set_entries(&self, entries: &[Entry]) {
        let list = self.entries();
        let mut existing: std::collections::HashMap<String, ClipEntry> = list
            .iter::<ClipEntry>()
            .filter_map(Result::ok)
            .map(|e| (e.id(), e))
            .collect();
        let items: Vec<ClipEntry> = entries
            .iter()
            .map(|e| existing.remove(&e.id).unwrap_or_else(|| ClipEntry::new(e)))
            .collect();
        list.splice(0, list.n_items(), &items);
    }

    fn update_empty_state(&self) {
        let empty = self.entries().n_items() == 0;
        self.imp()
            .stack
            .set_visible_child_name(if empty { "empty" } else { "list" });
    }

    fn find_entry(&self, id: &str) -> Option<(u32, ClipEntry)> {
        self.entries()
            .iter::<ClipEntry>()
            .filter_map(Result::ok)
            .zip(0..)
            .find(|(entry, _)| entry.id() == id)
            .map(|(entry, pos)| (pos, entry))
    }

    // ── Entry actions ───────────────────────────────────────────────────

    fn copy_entry_by_id(&self, id: &str) {
        if let Some((_, entry)) = self.find_entry(id) {
            self.copy_entry(&entry);
        }
    }

    /// Puts `entry` back on the clipboard and moves it to the top.
    fn copy_entry(&self, entry: &ClipEntry) {
        let Some(store) = self.imp().store.get().cloned() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[strong]
            entry,
            async move {
                let clipboard = win.clipboard();
                if entry.is_image() {
                    let Some(path) = entry.image_path().map(PathBuf::from) else {
                        return;
                    };
                    let Some(texture) = load_texture(path).await else {
                        return;
                    };
                    clipboard.set_texture(&texture);
                } else {
                    clipboard.set_text(&entry.text());
                }
                // Our own clipboard write is ignored by the monitor, so
                // promote the entry explicitly.
                let id = entry.id();
                match with_store(&store, move |s| s.promote(&id)).await {
                    // Select it at its new place at the top; a row's Copy
                    // button doesn't select the row by itself.
                    Ok(_) => win.load(Some(entry.id())),
                    Err(e) => log::warn!("could not move entry to the top: {e:#}"),
                }
            }
        ));
    }

    fn delete_entry(&self, id: &str) {
        let Some(store) = self.imp().store.get().cloned() else {
            return;
        };
        if let Some((pos, _)) = self.find_entry(id) {
            self.entries().remove(pos);
        }
        self.bump_change_seq();
        let id = id.to_owned();
        glib::spawn_future_local(async move {
            if let Err(e) = with_store(&store, move |s| s.delete(&id)).await {
                log::warn!("could not delete entry: {e:#}");
            }
        });
    }

    /// Asks for confirmation, then clears all history.
    pub fn confirm_clear(&self) {
        let dialog = adw::AlertDialog::new(
            Some("Clear all history?"),
            Some("All clipboard entries will be permanently deleted."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("clear", "Clear All")]);
        dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, response| {
                    if response == "clear" {
                        win.clear_history();
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }

    fn clear_history(&self) {
        let Some(store) = self.imp().store.get().cloned() else {
            return;
        };
        self.bump_change_seq();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                match with_store(&store, |s| s.clear()).await {
                    Ok(()) => {
                        win.entries().remove_all();
                        win.emit_by_name::<()>("history-cleared", &[]);
                    }
                    Err(e) => log::warn!("could not clear history: {e:#}"),
                }
            }
        ));
    }

    // ── Search and help ─────────────────────────────────────────────────

    fn toggle_search(&self) {
        let button = &self.imp().search_button;
        button.set_active(!button.is_active());
    }

    fn show_help(&self) {
        let builder = gtk::Builder::from_resource("/app/clipbrain/shortcuts-dialog.ui");
        if let Some(dialog) = builder.object::<adw::ShortcutsDialog>("shortcuts_dialog") {
            dialog.present(Some(self));
        }
    }

    // ── Keyboard ────────────────────────────────────────────────────────

    fn setup_keys(&self) {
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        controller.connect_key_pressed(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| win.handle_key(key, state)
        ));
        self.add_controller(controller);
    }

    /// Context-dependent keys. Ctrl+F, F1 and Ctrl+Shift+Delete are
    /// accelerators on the `win.*` actions instead.
    fn handle_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        use gdk::Key;
        use glib::Propagation::{Proceed, Stop};

        let modified = state.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK,
        );
        if let Some(dialog) = self.visible_dialog() {
            // The preview takes a few keys; other dialogs (clear
            // confirmation, shortcuts) handle their own.
            return match dialog.downcast::<PreviewDialog>() {
                Ok(preview) if !modified => self.handle_preview_key(&preview, key),
                _ => Proceed,
            };
        }
        if modified {
            return Proceed;
        }
        let searching = self.imp().search_bar.is_search_mode();
        let typing = searching && self.is_search_focused();

        match key {
            Key::slash if !searching => self.imp().search_button.set_active(true),
            Key::Escape if searching => self.imp().search_button.set_active(false),
            Key::Escape => self.set_visible(false),
            Key::Down => self.move_selection(1),
            Key::Up => self.move_selection(-1),
            Key::j if !typing => self.move_selection(1),
            Key::k if !typing => self.move_selection(-1),
            Key::Return | Key::KP_Enter => {
                if let Some(entry) = self.selected_entry() {
                    self.copy_entry(&entry);
                }
            }
            Key::Delete | Key::KP_Delete if !typing => {
                if let Some(entry) = self.selected_entry() {
                    self.delete_entry(&entry.id());
                }
            }
            Key::question if !typing => self.show_help(),
            Key::space if !typing => {
                if let Some(entry) = self.selected_entry() {
                    PreviewDialog::new(&entry).present(Some(self));
                }
            }
            _ => return Proceed,
        }
        Stop
    }

    /// Keys while the preview is open. Escape closes it (AdwDialog's own
    /// binding); arrows and Page Up/Down scroll the text.
    fn handle_preview_key(&self, preview: &PreviewDialog, key: gdk::Key) -> glib::Propagation {
        use gdk::Key;
        use glib::Propagation::{Proceed, Stop};

        match key {
            Key::space => {
                preview.close();
            }
            Key::Return | Key::KP_Enter => {
                if let Some(entry) = preview.entry() {
                    self.copy_entry(&entry);
                }
            }
            Key::j | Key::k => {
                self.move_selection(if key == Key::j { 1 } else { -1 });
                if let Some(entry) = self.selected_entry() {
                    preview.show_entry(&entry);
                }
            }
            _ => return Proceed,
        }
        Stop
    }

    fn is_search_focused(&self) -> bool {
        let entry = self.imp().search_entry.upcast_ref::<gtk::Widget>();
        gtk::prelude::GtkWindowExt::focus(self)
            .is_some_and(|focus| &focus == entry || focus.is_ancestor(entry))
    }

    fn selected_entry(&self) -> Option<ClipEntry> {
        self.selection().selected_item().and_downcast::<ClipEntry>()
    }

    /// Selects the visible entry with `id` and scrolls to it.
    fn select_entry(&self, id: &str) {
        let selection = self.selection();
        let pos = selection
            .iter::<glib::Object>()
            .filter_map(Result::ok)
            .position(|item| {
                item.downcast_ref::<ClipEntry>()
                    .is_some_and(|entry| entry.id() == id)
            });
        let Some(pos) = pos.and_then(|p| u32::try_from(p).ok()) else {
            return;
        };
        selection.set_selected(pos);
        self.imp()
            .list_view
            .scroll_to(pos, gtk::ListScrollFlags::NONE, None);
    }

    fn move_selection(&self, delta: i32) {
        let selection = self.selection();
        let current = Some(selection.selected()).filter(|&p| p != gtk::INVALID_LIST_POSITION);
        if let Some(pos) = next_position(current, selection.n_items(), delta) {
            selection.set_selected(pos);
            self.imp()
                .list_view
                .scroll_to(pos, gtk::ListScrollFlags::NONE, None);
        }
    }
}

/// Where the selection moves for `delta`, clamped to the list. With nothing
/// selected, moving down selects the first item and up the last.
fn next_position(current: Option<u32>, len: u32, delta: i32) -> Option<u32> {
    if len == 0 {
        return None;
    }
    Some(match current {
        None if delta > 0 => 0,
        None => len - 1,
        Some(pos) => pos.saturating_add_signed(delta).min(len - 1),
    })
}

#[cfg(test)]
mod tests;
