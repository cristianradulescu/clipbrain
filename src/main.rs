//! Clipbrain: a clipboard history manager for GNOME.

mod app;
mod entry;
mod monitor;
mod storage;
mod tray;
mod window;

use gtk::{gio, glib, prelude::*};

fn main() -> glib::ExitCode {
    // Run through XWayland. On native Wayland, mutter only sends clipboard
    // updates to the focused client, so copies made while the window is
    // hidden would be missed; X11 clients see every selection change.
    // An explicit GDK_BACKEND still wins.
    if std::env::var_os("GDK_BACKEND").is_none() {
        // SAFETY: nothing else is running yet; GTK and logging start below.
        unsafe { std::env::set_var("GDK_BACKEND", "x11") };
    }
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("CLIPBRAIN_LOG", "warn")).init();

    gio::resources_register_include!("clipbrain.gresource").expect("bundled resources are valid");

    app::ClipbrainApp::new().run()
}
