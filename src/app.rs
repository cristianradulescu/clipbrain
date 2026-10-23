//! The application object: owns the history store and clipboard monitor.

use std::sync::{Arc, Mutex, PoisonError};

use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib, prelude::*};

use crate::monitor::{Capture, GdkClipboardSource, Monitor};
use crate::storage::Store;

/// Unique application id, shared with the Python version so a second
/// launch activates whichever instance is running.
pub const APP_ID: &str = "app.clipbrain";

/// The history store, shared with blocking worker threads.
pub type SharedStore = Arc<Mutex<Store>>;

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};

    use super::*;

    #[derive(Default)]
    pub struct ClipbrainApp {
        /// Keeps the app alive while no window is shown.
        pub hold: RefCell<Option<gio::ApplicationHoldGuard>>,
        pub started: Cell<bool>,
        pub store: OnceCell<SharedStore>,
        pub monitor: OnceCell<Monitor<GdkClipboardSource>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClipbrainApp {
        const NAME: &'static str = "ClipbrainApp";
        type Type = super::ClipbrainApp;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for ClipbrainApp {}

    impl ApplicationImpl for ClipbrainApp {
        fn startup(&self) {
            self.parent_startup();
            self.hold.replace(Some(self.obj().hold()));
        }

        fn activate(&self) {
            self.parent_activate();
            self.obj().on_activate();
        }
    }

    impl GtkApplicationImpl for ClipbrainApp {}
    impl AdwApplicationImpl for ClipbrainApp {}
}

glib::wrapper! {
    /// Clipbrain's `AdwApplication`.
    pub struct ClipbrainApp(ObjectSubclass<imp::ClipbrainApp>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl ClipbrainApp {
    /// Creates the application.
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", APP_ID)
            .build()
    }

    fn on_activate(&self) {
        if self.imp().started.replace(true) {
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            async move { app.start().await }
        ));
    }

    /// Opens the store off-thread, then starts watching the clipboard.
    async fn start(&self) {
        let store = match gio::spawn_blocking(Store::open_default).await {
            Ok(Ok(store)) => store,
            Ok(Err(e)) => return self.fail(&format!("could not open clipboard history: {e}")),
            Err(_) => return self.fail("opening clipboard history panicked"),
        };
        log::info!("loaded {} history entries", store.entries().len());
        let seed = store.seed_hash();
        let store = Arc::new(Mutex::new(store));
        let _ = self.imp().store.set(Arc::clone(&store));

        let Some(display) = gdk::Display::default() else {
            return self.fail("no display available");
        };
        let app = self.downgrade();
        let monitor = Monitor::new(
            GdkClipboardSource::new(display.clipboard()),
            seed,
            move |capture| {
                if let Some(app) = app.upgrade() {
                    app.save_capture(capture);
                }
            },
        );
        monitor.start();
        let _ = self.imp().monitor.set(monitor);
    }

    /// Stores a capture on a worker thread.
    fn save_capture(&self, capture: Capture) {
        let Some(store) = self.imp().store.get().cloned() else {
            return;
        };
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
                match capture {
                    Capture::Text(text) => store.add_text(&text),
                    Capture::Image(png) => store.add_image(&png),
                }
            })
            .await;
            match result {
                Ok(Ok(entry)) => log::info!("stored {:?} entry {}", entry.kind, entry.id),
                Ok(Err(e)) => log::warn!("failed to store clipboard entry: {e}"),
                Err(_) => log::error!("storage worker panicked"),
            }
        });
    }

    fn fail(&self, message: &str) {
        log::error!("{message}");
        self.imp().hold.take();
        self.quit();
    }
}

impl Default for ClipbrainApp {
    fn default() -> Self {
        Self::new()
    }
}
