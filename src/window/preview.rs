//! `PreviewWindow`: full view of one history entry, opened with Space.
//!
//! A separate modal window rather than an `AdwDialog`, because a dialog
//! can't grow beyond the (small) history window and images need the room.

use std::path::PathBuf;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};

use super::HistoryWindow;
use crate::entry::{ClipEntry, load_texture};

/// Text beyond this many bytes isn't shown (a GtkTextView gets sluggish
/// with several MB); copying still copies everything.
const MAX_PREVIEW_BYTES: usize = 1024 * 1024;
/// Size of the first preview; later ones reuse the last size.
pub const DEFAULT_SIZE: (i32, i32) = (700, 500);
/// The preview never shrinks below this.
const MIN_SIZE: (i32, i32) = (320, 240);
/// Each `+` / `-` press scales the window by this factor.
pub const RESIZE_STEP: f64 = 1.1;

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/app/clipbrain/preview-window.ui")]
    pub struct PreviewWindow {
        #[template_child]
        pub title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub copy_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub text_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub picture: TemplateChild<gtk::Picture>,
        /// The entry being shown.
        pub entry: RefCell<Option<ClipEntry>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreviewWindow {
        const NAME: &'static str = "ClipbrainPreviewWindow";
        type Type = super::PreviewWindow;
        type ParentType = adw::Window;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PreviewWindow {}
    impl WidgetImpl for PreviewWindow {}
    impl WindowImpl for PreviewWindow {}
    impl AdwWindowImpl for PreviewWindow {}
}

glib::wrapper! {
    /// Shows a text entry in full, or an image entry at full size.
    pub struct PreviewWindow(ObjectSubclass<imp::PreviewWindow>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl PreviewWindow {
    /// Creates a preview of `entry`, modal over `parent`, at `size`. Keys
    /// are handed to the parent, which owns navigation and copying.
    pub fn new(parent: &HistoryWindow, entry: &ClipEntry, size: (i32, i32)) -> Self {
        let preview: Self = glib::Object::builder()
            .property("transient-for", parent)
            .property("default-width", size.0)
            .property("default-height", size.1)
            .build();

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            parent,
            #[weak]
            preview,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| parent.handle_preview_key(&preview, key, state)
        ));
        preview.add_controller(keys);

        preview.imp().copy_button.connect_clicked(glib::clone!(
            #[weak]
            parent,
            #[weak]
            preview,
            move |_| {
                if let Some(entry) = preview.entry() {
                    parent.copy_entry(&entry);
                }
            }
        ));

        preview.show_entry(entry);
        preview
    }

    /// Scales the window by `factor`, keeping it between the minimum size
    /// and the monitor's size.
    pub fn resize_by(&self, factor: f64) {
        let max = self
            .surface()
            .and_then(|surface| WidgetExt::display(self).monitor_at_surface(&surface))
            .map(|monitor| {
                let geometry = monitor.geometry();
                (geometry.width(), geometry.height())
            });
        let (width, height) = scaled_size(self.default_size(), factor, MIN_SIZE, max);
        self.set_default_size(width, height);
    }

    /// The entry being shown.
    pub fn entry(&self) -> Option<ClipEntry> {
        self.imp().entry.borrow().clone()
    }

    /// Switches the preview to `entry`.
    pub fn show_entry(&self, entry: &ClipEntry) {
        let imp = self.imp();
        imp.entry.replace(Some(entry.clone()));

        if !entry.is_image() {
            let text = entry.text();
            let (shown, truncated) = preview_text(&text, MAX_PREVIEW_BYTES);
            imp.title.set_title("Text");
            imp.title.set_subtitle(&describe_text(&text, truncated));
            let buffer = imp.text_view.buffer();
            buffer.set_text(shown);
            // set_text leaves the cursor at the end; start at the top.
            buffer.place_cursor(&buffer.start_iter());
            if let Some(adjustment) = imp.text_view.vadjustment() {
                adjustment.set_value(0.0);
            }
            imp.stack.set_visible_child_name("text");
            return;
        }

        imp.title.set_title("Image");
        imp.title.set_subtitle("");
        imp.picture.set_paintable(None::<&gdk::Paintable>);
        imp.stack.set_visible_child_name("loading");
        let Some(path) = entry.image_path().map(PathBuf::from) else {
            imp.stack.set_visible_child_name("missing");
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = preview)]
            self,
            #[strong]
            entry,
            async move {
                let texture = load_texture(path).await;
                // The user may have moved on to another entry meanwhile.
                if preview.entry().as_ref() != Some(&entry) {
                    return;
                }
                let imp = preview.imp();
                match texture {
                    Some(texture) => {
                        imp.title.set_subtitle(&format!(
                            "{} × {} px",
                            texture.width(),
                            texture.height()
                        ));
                        imp.picture.set_paintable(Some(&texture));
                        imp.stack.set_visible_child_name("image");
                    }
                    None => imp.stack.set_visible_child_name("missing"),
                }
            }
        ));
    }

    /// The text currently in the text view (for tests).
    #[cfg(test)]
    pub fn shown_text(&self) -> String {
        let buffer = self.imp().text_view.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .into()
    }

    /// The visible page: "text", "image", "loading" or "missing" (for tests).
    #[cfg(test)]
    pub fn page(&self) -> String {
        self.imp()
            .stack
            .visible_child_name()
            .map(Into::into)
            .unwrap_or_default()
    }
}

/// `size` scaled by `factor`, at least `min` and at most `max` (if known).
fn scaled_size(
    size: (i32, i32),
    factor: f64,
    min: (i32, i32),
    max: Option<(i32, i32)>,
) -> (i32, i32) {
    let scale = |value: i32, min: i32, max: Option<i32>| {
        let scaled = (f64::from(value) * factor).round() as i32;
        let scaled = scaled.max(min);
        max.map_or(scaled, |max| scaled.min(max.max(min)))
    };
    (
        scale(size.0, min.0, max.map(|m| m.0)),
        scale(size.1, min.1, max.map(|m| m.1)),
    )
}

/// The part of `text` to display: at most `limit` bytes, cut at a
/// character boundary. Returns whether it was truncated.
fn preview_text(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    (&text[..text.floor_char_boundary(limit)], true)
}

/// Header subtitle for a text entry, e.g. "3 lines · 1.2 kB".
fn describe_text(text: &str, truncated: bool) -> String {
    let lines = text.lines().count().max(1);
    let noun = if lines == 1 { "line" } else { "lines" };
    let size = glib::format_size(text.len() as u64);
    let mut description = format!("{lines} {noun} · {size}");
    if truncated {
        description.push_str(" · showing the first 1 MB");
    }
    description
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizing_scales_by_the_factor() {
        assert_eq!(
            scaled_size((700, 500), RESIZE_STEP, MIN_SIZE, None),
            (770, 550)
        );
        assert_eq!(
            scaled_size((770, 550), 1.0 / RESIZE_STEP, MIN_SIZE, None),
            (700, 500)
        );
    }

    #[test]
    fn resizing_is_clamped() {
        assert_eq!(
            scaled_size((1900, 1000), RESIZE_STEP, MIN_SIZE, Some((1920, 1080))),
            (1920, 1080)
        );
        assert_eq!(scaled_size((330, 250), 0.5, MIN_SIZE, None), MIN_SIZE);
        // A monitor smaller than the minimum doesn't win over the minimum.
        assert_eq!(
            scaled_size((400, 300), 1.0, MIN_SIZE, Some((200, 100))),
            MIN_SIZE
        );
    }

    #[test]
    fn short_text_is_shown_whole() {
        assert_eq!(preview_text("abc", 10), ("abc", false));
        assert_eq!(preview_text("abc", 3), ("abc", false));
    }

    #[test]
    fn long_text_is_cut_at_a_char_boundary() {
        // "é" is 2 bytes; a 3-byte limit must not split the second one.
        assert_eq!(preview_text("éé", 3), ("é", true));
        assert_eq!(preview_text("abcdef", 4), ("abcd", true));
    }

    #[test]
    fn description_counts_lines_and_size() {
        assert_eq!(describe_text("one", false), "1 line · 3 bytes");
        assert_eq!(describe_text("", false), "1 line · 0 bytes");
        assert_eq!(describe_text("a\nb\nc\n", false), "3 lines · 6 bytes");
        assert!(describe_text("x", true).ends_with("showing the first 1 MB"));
    }
}
