//! System tray icon: a StatusNotifierItem served over D-Bus by `ksni`.
//!
//! ksni runs the service on its own thread. Clicks are forwarded to the GTK
//! main loop as [`TrayEvent`]s over an `async_channel`.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use ksni::{MenuItem, OfflineReason, ToolTip, Tray};

/// A tray interaction, to be handled on the GTK main thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// Show the history window.
    Show,
    /// Ask to clear all history.
    ClearHistory,
    /// Quit the application.
    Quit,
}

/// Handle to the running tray service.
pub type TrayHandle = Handle<ClipbrainTray>;

/// The tray icon and its menu.
#[derive(Debug)]
pub struct ClipbrainTray {
    events: async_channel::Sender<TrayEvent>,
}

impl ClipbrainTray {
    fn send(&self, event: TrayEvent) {
        if self.events.try_send(event).is_err() {
            log::debug!("tray event {event:?} dropped: app is shutting down");
        }
    }
}

impl Tray for ClipbrainTray {
    fn id(&self) -> String {
        "clipbrain".into()
    }

    fn title(&self) -> String {
        "Clipbrain".into()
    }

    fn icon_name(&self) -> String {
        "edit-paste-symbolic".into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Clipbrain".into(),
            description: "Clipboard history".into(),
            ..ToolTip::default()
        }
    }

    /// Left click on the icon.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayEvent::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Show History".into(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayEvent::Show)),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Clear History…".into(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayEvent::ClearHistory)),
                ..StandardItem::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayEvent::Quit)),
                ..StandardItem::default()
            }
            .into(),
        ]
    }

    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        // Keep the service alive; the icon reappears if the watcher (e.g. the
        // AppIndicator extension) comes back.
        log::warn!("tray host went offline: {reason:?}");
        true
    }
}

/// Starts the tray service. Blocks while connecting to D-Bus, so call it
/// off the main thread. Returns `None` (after logging) if it can't start.
pub fn spawn(events: async_channel::Sender<TrayEvent>) -> Option<TrayHandle> {
    // Assume a tray host exists even if it hasn't registered yet: at login
    // the AppIndicator extension can start after us.
    ClipbrainTray { events }
        .assume_sni_available(true)
        .spawn()
        .inspect_err(|e| log::warn!("could not start the tray icon: {e}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tray() -> (ClipbrainTray, async_channel::Receiver<TrayEvent>) {
        let (events, rx) = async_channel::unbounded();
        (ClipbrainTray { events }, rx)
    }

    #[test]
    fn clicking_the_icon_shows_the_window() {
        let (mut tray, rx) = tray();
        tray.activate(0, 0);
        assert_eq!(rx.try_recv(), Ok(TrayEvent::Show));
    }

    #[test]
    fn menu_items_send_their_events() {
        let (mut tray, rx) = tray();
        let mut labels = Vec::new();
        for item in tray.menu() {
            match item {
                MenuItem::Standard(item) => {
                    labels.push(item.label.clone());
                    (item.activate)(&mut tray);
                }
                MenuItem::Separator => labels.push("---".into()),
                other => panic!("unexpected menu item {other:?}"),
            }
        }
        assert_eq!(labels, ["Show History", "Clear History…", "---", "Quit"]);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            events,
            [TrayEvent::Show, TrayEvent::ClearHistory, TrayEvent::Quit]
        );
    }

    #[test]
    fn events_after_shutdown_are_dropped_quietly() {
        let (tray, rx) = tray();
        drop(rx);
        tray.send(TrayEvent::Show);
    }
}
