//! The application object: wires the history store, clipboard monitor and
//! window together.

use std::sync::{Arc, Mutex, PoisonError};

use adw::subclass::prelude::*;
use anyhow::anyhow;
use gtk::{gdk, gio, glib, prelude::*};

use crate::monitor::{Capture, GdkClipboardSource, Monitor};
use crate::storage::{StorageError, Store};
use crate::tray::{self, TrayEvent, TrayHandle};
use crate::window::HistoryWindow;

/// Unique application id, shared with the Python version so a second
/// launch activates whichever instance is running.
pub const APP_ID: &str = "app.clipbrain";

/// The history store, shared with blocking worker threads.
pub type SharedStore = Arc<Mutex<Store>>;

/// Runs `f` against the store on GIO's blocking thread pool, keeping file
/// I/O off the main thread.
pub async fn with_store<T, F>(store: &SharedStore, f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut Store) -> Result<T, StorageError> + Send + 'static,
{
    let store = Arc::clone(store);
    let result = gio::spawn_blocking(move || {
        let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut store)
    })
    .await
    .map_err(|_| anyhow!("storage worker panicked"))?;
    Ok(result?)
}

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};

    use super::*;

    #[derive(Default)]
    pub struct ClipbrainApp {
        /// Keeps the app alive while the window is hidden.
        pub hold: RefCell<Option<gio::ApplicationHoldGuard>>,
        pub started: Cell<bool>,
        pub store: OnceCell<SharedStore>,
        pub monitor: OnceCell<Monitor<GdkClipboardSource>>,
        pub window: OnceCell<HistoryWindow>,
        pub tray: RefCell<Option<TrayHandle>>,
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
            let app = self.obj();
            self.hold.replace(Some(app.hold()));
            gtk::Window::set_default_icon_name(APP_ID);
            app.setup_actions();
        }

        fn activate(&self) {
            self.parent_activate();
            self.obj().on_activate();
        }

        fn shutdown(&self) {
            // Ask the tray thread to stop; don't wait for it on the way out.
            if let Some(tray) = self.tray.take() {
                drop(tray.shutdown());
            }
            self.parent_shutdown();
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

    fn setup_actions(&self) {
        let show = gio::ActionEntry::builder("show-window")
            .activate(|app: &Self, _, _| app.show_window())
            .build();
        let quit = gio::ActionEntry::builder("quit")
            .activate(|app: &Self, _, _| app.quit_app())
            .build();
        self.add_action_entries([show, quit]);

        self.set_accels_for_action("app.show-window", &["<Control><Shift>v"]);
        self.set_accels_for_action("win.toggle-search", &["<Control>f"]);
        self.set_accels_for_action("win.show-help", &["F1"]);
        self.set_accels_for_action("win.clear-history", &["<Control><Shift>Delete"]);
    }

    fn on_activate(&self) {
        if self.imp().started.replace(true) {
            // Already running (e.g. a second `clipbrain` launch): show it.
            self.show_window();
            return;
        }
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            async move { app.start().await }
        ));
    }

    /// Opens the store off-thread, then starts the monitor and window.
    async fn start(&self) {
        let store = match gio::spawn_blocking(Store::open_default).await {
            Ok(Ok(store)) => store,
            Ok(Err(e)) => return self.fail(&format!("could not open clipboard history: {e}")),
            Err(_) => return self.fail("opening clipboard history panicked"),
        };
        log::info!("loaded {} history entries", store.entries().len());
        let seed = store.seed_hash();
        let store = Arc::new(Mutex::new(store));
        let imp = self.imp();
        let _ = imp.store.set(Arc::clone(&store));

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
        let _ = imp.monitor.set(monitor);

        let window = HistoryWindow::new(store);
        window.set_application(Some(self));
        let app = self.downgrade();
        window.connect_history_cleared(move || {
            if let Some(app) = app.upgrade()
                && let Some(monitor) = app.imp().monitor.get()
            {
                monitor.reset_last_hash();
            }
        });
        let _ = imp.window.set(window);
        self.show_window();
        self.start_tray();
    }

    /// Starts the tray icon off-thread and handles its events here.
    fn start_tray(&self) {
        let (events, received) = async_channel::unbounded();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                match gio::spawn_blocking(move || tray::spawn(events)).await {
                    Ok(Some(handle)) => {
                        app.imp().tray.replace(Some(handle));
                    }
                    // Already logged; the app keeps working without a tray.
                    Ok(None) => return,
                    Err(_) => return log::error!("tray startup panicked"),
                }
                while let Ok(event) = received.recv().await {
                    app.on_tray_event(event);
                }
            }
        ));
    }

    fn on_tray_event(&self, event: TrayEvent) {
        log::debug!("tray: {event:?}");
        match event {
            TrayEvent::Show => self.show_window(),
            TrayEvent::ClearHistory => {
                self.show_window();
                if let Some(window) = self.imp().window.get() {
                    window.confirm_clear();
                }
            }
            TrayEvent::Quit => self.quit_app(),
        }
    }

    /// Stops the tray, releases the hold and quits.
    fn quit_app(&self) {
        let tray = self.imp().tray.take();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                if let Some(tray) = tray {
                    // Removes the icon before we exit; waiting blocks, so do
                    // it on a worker thread.
                    let _ = gio::spawn_blocking(move || tray.shutdown().wait()).await;
                }
                app.imp().hold.take();
                app.quit();
            }
        ));
    }

    /// Reloads and presents the history window.
    fn show_window(&self) {
        if let Some(window) = self.imp().window.get() {
            window.reload();
            window.set_visible(true);
            window.present();
        }
    }

    /// Stores a capture off-thread; refreshes the window if it's showing.
    fn save_capture(&self, capture: Capture) {
        let Some(store) = self.imp().store.get().cloned() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            async move {
                let result = with_store(&store, move |s| match capture {
                    Capture::Text(text) => s.add_text(&text),
                    Capture::Image(png) => s.add_image(&png),
                })
                .await;
                match result {
                    Ok(entry) => log::info!("stored {:?} entry {}", entry.kind, entry.id),
                    Err(e) => return log::warn!("failed to store clipboard entry: {e:#}"),
                }
                if let Some(window) = app.imp().window.get().filter(|w| w.is_visible()) {
                    window.reload();
                }
            }
        ));
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
