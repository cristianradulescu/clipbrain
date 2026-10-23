//! `EntryRow`: one row of the history list.

use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::entry::{ClipEntry, Thumbnail, text_preview, text_tooltip};

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/app/clipbrain/entry-row.ui")]
    pub struct EntryRow {
        #[template_child]
        pub thumbnail: TemplateChild<gtk::Image>,
        #[template_child]
        pub label: TemplateChild<gtk::Label>,
        #[template_child]
        pub copy_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub delete_button: TemplateChild<gtk::Button>,
        /// The entry currently shown, if bound.
        pub entry: RefCell<Option<ClipEntry>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EntryRow {
        const NAME: &'static str = "ClipbrainEntryRow";
        type Type = super::EntryRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for EntryRow {}
    impl WidgetImpl for EntryRow {}
    impl BoxImpl for EntryRow {}
}

glib::wrapper! {
    /// A history list row: thumbnail, label, Copy and Delete buttons.
    pub struct EntryRow(ObjectSubclass<imp::EntryRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl EntryRow {
    /// Creates an unbound row.
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Shows `entry`. Image thumbnails load lazily on first bind.
    pub fn bind(&self, entry: &ClipEntry) {
        let imp = self.imp();
        imp.entry.replace(Some(entry.clone()));
        let target = entry.id().to_variant();
        imp.copy_button.set_action_target_value(Some(&target));
        imp.delete_button.set_action_target_value(Some(&target));

        if !entry.is_image() {
            let text = entry.text();
            imp.thumbnail.set_visible(false);
            imp.label.set_text(&text_preview(&text));
            imp.label.set_tooltip_text(Some(&text_tooltip(&text)));
            return;
        }

        imp.thumbnail.set_visible(true);
        imp.label.set_text("Image");
        imp.label.set_tooltip_text(None);
        if let Some(thumbnail) = entry.cached_thumbnail() {
            self.show_thumbnail(&thumbnail);
            return;
        }
        imp.thumbnail
            .set_icon_name(Some("image-x-generic-symbolic"));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = row)]
            self,
            #[strong]
            entry,
            async move {
                let thumbnail = entry.thumbnail().await;
                // The row may have been recycled for another entry meanwhile.
                if row.imp().entry.borrow().as_ref() == Some(&entry) {
                    row.show_thumbnail(&thumbnail);
                }
            }
        ));
    }

    /// Clears the row before it is recycled.
    pub fn unbind(&self) {
        let imp = self.imp();
        imp.entry.replace(None);
        imp.label.set_text("");
        imp.label.set_tooltip_text(None);
        imp.thumbnail.clear();
        imp.thumbnail.set_visible(true);
    }

    fn show_thumbnail(&self, thumbnail: &Thumbnail) {
        let imp = self.imp();
        match thumbnail {
            Thumbnail::Loaded {
                texture,
                width,
                height,
            } => {
                imp.thumbnail.set_paintable(Some(texture));
                imp.label.set_text(&format!("Image  {width} × {height} px"));
            }
            Thumbnail::Missing => imp.thumbnail.set_icon_name(Some("image-missing-symbolic")),
            Thumbnail::Failed => imp
                .thumbnail
                .set_icon_name(Some("image-x-generic-symbolic")),
        }
    }
}

impl Default for EntryRow {
    fn default() -> Self {
        Self::new()
    }
}
