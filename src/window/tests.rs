use super::*;

#[test]
fn next_position_clamps_and_starts_at_the_ends() {
    assert_eq!(next_position(None, 0, 1), None);
    assert_eq!(next_position(None, 3, 1), Some(0));
    assert_eq!(next_position(None, 3, -1), Some(2));
    assert_eq!(next_position(Some(0), 3, 1), Some(1));
    assert_eq!(next_position(Some(2), 3, 1), Some(2));
    assert_eq!(next_position(Some(0), 3, -1), Some(0));
}

mod ui {
    //! Drives the real window. Needs a display, so it is ignored by default;
    //! run it headless with:
    //!
    //! ```sh
    //! gtk4-broadwayd :9 &
    //! GDK_BACKEND=broadway BROADWAY_DISPLAY=:9 cargo test -- --ignored
    //! ```

    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use gtk::glib::Propagation;

    use super::super::*;
    use crate::entry::Thumbnail;
    use crate::storage::Store;

    const FIXTURE_PNG: &[u8] = include_bytes!("../../tests/fixtures/python_image.png");

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let ctx = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            while ctx.iteration(false) {}
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn press(win: &HistoryWindow, key: gdk::Key) -> Propagation {
        win.handle_key(key, gdk::ModifierType::empty())
    }

    fn texts(win: &HistoryWindow) -> Vec<String> {
        win.selection()
            .iter::<glib::Object>()
            .filter_map(Result::ok)
            .filter_map(|o| o.downcast::<ClipEntry>().ok())
            .map(|e| {
                if e.is_image() {
                    "<image>".into()
                } else {
                    e.text()
                }
            })
            .collect()
    }

    #[test]
    #[ignore = "needs a display; see module docs"]
    fn history_window() {
        gtk::init().unwrap();
        adw::init().unwrap();
        gio::resources_register_include!("clipbrain.gresource").unwrap();

        let dir = tempfile::TempDir::new().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.add_image(FIXTURE_PNG).unwrap();
        store.add_text("Second entry\nwith a second line").unwrap();
        store.add_text("alpha").unwrap();
        let store = Arc::new(Mutex::new(store));
        let top_id = || store.lock().unwrap().entries()[0].id.clone();

        let win = HistoryWindow::new(Arc::clone(&store));
        let cleared = Rc::new(Cell::new(false));
        win.connect_history_cleared(glib::clone!(
            #[strong]
            cleared,
            move || cleared.set(true)
        ));
        win.present();
        assert_eq!(win.imp().stack.visible_child_name().unwrap(), "empty");

        // Reload fills the list and lazily loads the image thumbnail.
        win.reload();
        wait_until("reload", || win.entries().n_items() == 3);
        assert_eq!(win.imp().stack.visible_child_name().unwrap(), "list");
        assert_eq!(
            texts(&win),
            ["alpha", "Second entry\nwith a second line", "<image>"]
        );
        let image = win.entries().item(2).and_downcast::<ClipEntry>().unwrap();
        wait_until("thumbnail", || image.cached_thumbnail().is_some());
        assert!(matches!(
            image.cached_thumbnail(),
            Some(Thumbnail::Loaded {
                width: 2,
                height: 2,
                ..
            })
        ));

        // j/k and arrows move the selection, clamped at the ends.
        let selected = || win.selection().selected();
        assert_eq!(selected(), gtk::INVALID_LIST_POSITION);
        press(&win, gdk::Key::j);
        assert_eq!(selected(), 0);
        press(&win, gdk::Key::Down);
        press(&win, gdk::Key::Down);
        press(&win, gdk::Key::Down);
        assert_eq!(selected(), 2);
        press(&win, gdk::Key::k);
        press(&win, gdk::Key::Up);
        press(&win, gdk::Key::Up);
        assert_eq!(selected(), 0);

        // `/` opens search; typing filters; j/k go to the entry while typing.
        assert_eq!(press(&win, gdk::Key::slash), Propagation::Stop);
        assert!(win.imp().search_bar.is_search_mode());
        wait_until("search focus", || win.is_search_focused());
        assert_eq!(press(&win, gdk::Key::j), Propagation::Proceed);
        assert_eq!(press(&win, gdk::Key::question), Propagation::Proceed);
        assert_eq!(press(&win, gdk::Key::Delete), Propagation::Proceed);
        win.imp().search_entry.set_text("  SECOND ");
        wait_until("text filter", || texts(&win).len() == 1);
        assert_eq!(texts(&win), ["Second entry\nwith a second line"]);
        win.imp().search_entry.set_text("ima");
        wait_until("image filter", || texts(&win) == ["<image>"]);

        // Escape closes search (clearing the query) without hiding.
        assert_eq!(press(&win, gdk::Key::Escape), Propagation::Stop);
        assert!(!win.imp().search_bar.is_search_mode());
        wait_until("filter reset", || texts(&win).len() == 3);
        assert!(win.is_visible());

        // Enter copies the selection to the clipboard and moves it to the top.
        press(&win, gdk::Key::Down); // nothing selected after filtering → first
        press(&win, gdk::Key::Down);
        let second = win.selected_entry().unwrap();
        assert_eq!(second.text(), "Second entry\nwith a second line");
        press(&win, gdk::Key::Return);
        wait_until("promote", || top_id() == second.id());
        wait_until("list reorder", || texts(&win)[0] == second.text());
        let clipboard_text = Rc::new(std::cell::RefCell::new(None));
        glib::spawn_future_local(glib::clone!(
            #[strong]
            clipboard_text,
            #[strong(rename_to = clipboard)]
            win.clipboard(),
            async move {
                let text = clipboard.read_text_future().await.ok().flatten();
                clipboard_text.replace(text.map(String::from));
            }
        ));
        wait_until("clipboard read", || clipboard_text.borrow().is_some());
        assert_eq!(
            clipboard_text.borrow().as_deref(),
            Some(second.text().as_str())
        );

        // The row's Copy button action also copies, by id.
        let alpha = win
            .entries()
            .iter::<ClipEntry>()
            .filter_map(Result::ok)
            .find(|e| e.text() == "alpha")
            .unwrap();
        WidgetExt::activate_action(&win, "win.copy-entry", Some(&alpha.id().to_variant())).unwrap();
        wait_until("copy action", || top_id() == alpha.id());

        // Delete removes the selected entry from the list and storage.
        wait_until("list settles", || texts(&win)[0] == "alpha");
        win.selection().set_selected(2);
        let victim = win.selected_entry().unwrap();
        assert!(victim.is_image());
        assert_eq!(press(&win, gdk::Key::Delete), Propagation::Stop);
        assert_eq!(texts(&win).len(), 2);
        wait_until("storage delete", || {
            store.lock().unwrap().entries().len() == 2
        });

        // `?` shows the shortcuts dialog; list keys are ignored while open.
        press(&win, gdk::Key::question);
        let dialog = win.visible_dialog().expect("shortcuts dialog");
        assert!(dialog.is::<adw::ShortcutsDialog>());
        let before = selected();
        assert_eq!(press(&win, gdk::Key::j), Propagation::Proceed);
        assert_eq!(selected(), before);
        wait_until("dialog mapped", || dialog.is_mapped());
        // Escape in an AdwDialog runs close(), the path that crashes the
        // Python app's GtkShortcutsWindow on GTK 4.22.
        assert!(dialog.close());
        wait_until("dialog closed", || win.visible_dialog().is_none());

        // Clear-history asks first; confirming empties list and storage.
        WidgetExt::activate_action(&win, "win.clear-history", None).unwrap();
        let alert = win
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>()
            .expect("confirmation dialog");
        assert_eq!(alert.default_response().as_deref(), Some("cancel"));
        wait_until("alert mapped", || alert.is_mapped());
        alert.emit_by_name::<()>("response", &[&"clear"]);
        wait_until("clear", || cleared.get());
        assert_eq!(win.entries().n_items(), 0);
        assert!(store.lock().unwrap().entries().is_empty());
        assert_eq!(win.imp().stack.visible_child_name().unwrap(), "empty");
        alert.force_close();
        wait_until("alert closed", || win.visible_dialog().is_none());

        // Escape with search closed hides; close-request hides, never destroys.
        press(&win, gdk::Key::Escape);
        assert!(!win.is_visible());
        win.present();
        win.close();
        assert!(!win.is_visible());
        win.present();
        assert!(win.is_visible());
    }
}
