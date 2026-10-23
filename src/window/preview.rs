//! `PreviewDialog`: full view of one history entry, opened with Space.

use std::path::PathBuf;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use crate::entry::{ClipEntry, load_texture};

/// Text beyond this many bytes isn't shown (a GtkTextView gets sluggish
/// with several MB); copying still copies everything.
const MAX_PREVIEW_BYTES: usize = 1024 * 1024;

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/app/clipbrain/preview-dialog.ui")]
    pub struct PreviewDialog {
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
    impl ObjectSubclass for PreviewDialog {
        const NAME: &'static str = "ClipbrainPreviewDialog";
        type Type = super::PreviewDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PreviewDialog {}
    impl WidgetImpl for PreviewDialog {}
    impl AdwDialogImpl for PreviewDialog {}
}

glib::wrapper! {
    /// Shows a text entry in full, or an image entry at full size.
    pub struct PreviewDialog(ObjectSubclass<imp::PreviewDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl PreviewDialog {
    /// Creates a dialog showing `entry`.
    pub fn new(entry: &ClipEntry) -> Self {
        let dialog: Self = glib::Object::new();
        dialog.show_entry(entry);
        dialog
    }

    /// The entry being shown.
    pub fn entry(&self) -> Option<ClipEntry> {
        self.imp().entry.borrow().clone()
    }

    /// Switches the dialog to `entry`.
    pub fn show_entry(&self, entry: &ClipEntry) {
        let imp = self.imp();
        imp.entry.replace(Some(entry.clone()));
        imp.copy_button
            .set_action_target_value(Some(&entry.id().to_variant()));

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
        imp.picture.set_paintable(None::<&gtk::gdk::Paintable>);
        imp.stack.set_visible_child_name("loading");
        let Some(path) = entry.image_path().map(PathBuf::from) else {
            imp.stack.set_visible_child_name("missing");
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[strong]
            entry,
            async move {
                let texture = load_texture(path).await;
                // The user may have moved on to another entry meanwhile.
                if dialog.entry().as_ref() != Some(&entry) {
                    return;
                }
                let imp = dialog.imp();
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
