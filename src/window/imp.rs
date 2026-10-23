use std::cell::{Cell, OnceCell};
use std::sync::OnceLock;

use adw::subclass::prelude::*;
use gtk::glib::subclass::Signal;
use gtk::{gio, glib, prelude::*};

use super::preview::{self, PreviewWindow};
use crate::app::SharedStore;
use crate::entry::ClipEntry;

#[derive(gtk::CompositeTemplate)]
#[template(resource = "/app/clipbrain/window.ui")]
pub struct HistoryWindow {
    #[template_child]
    pub search_button: TemplateChild<gtk::ToggleButton>,
    #[template_child]
    pub search_bar: TemplateChild<gtk::SearchBar>,
    #[template_child]
    pub search_entry: TemplateChild<gtk::SearchEntry>,
    #[template_child]
    pub stack: TemplateChild<gtk::Stack>,
    #[template_child]
    pub list_view: TemplateChild<gtk::ListView>,

    /// All entries, newest first.
    pub entries: gio::ListStore,
    /// Search filter; its match function is set in `setup_list`.
    pub filter: gtk::CustomFilter,
    /// Selection over the filtered entries.
    pub selection: gtk::SingleSelection,
    pub store: OnceCell<SharedStore>,
    /// Bumped by every reload and local edit; an async reload only applies
    /// its snapshot if no newer change happened while it was loading.
    pub change_seq: Cell<u64>,
    /// The open preview window, if any.
    pub preview: glib::WeakRef<PreviewWindow>,
    /// Size for the next preview: the last one's final size.
    pub preview_size: Cell<(i32, i32)>,
}

impl Default for HistoryWindow {
    fn default() -> Self {
        let entries = gio::ListStore::new::<ClipEntry>();
        let filter = gtk::CustomFilter::new(|_| true);
        let filtered = gtk::FilterListModel::new(Some(entries.clone()), Some(filter.clone()));
        let selection = gtk::SingleSelection::new(Some(filtered));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        Self {
            search_button: TemplateChild::default(),
            search_bar: TemplateChild::default(),
            search_entry: TemplateChild::default(),
            stack: TemplateChild::default(),
            list_view: TemplateChild::default(),
            entries,
            filter,
            selection,
            store: OnceCell::new(),
            change_seq: Cell::new(0),
            preview: glib::WeakRef::new(),
            preview_size: Cell::new(preview::DEFAULT_SIZE),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for HistoryWindow {
    const NAME: &'static str = "ClipbrainWindow";
    type Type = super::HistoryWindow;
    type ParentType = adw::ApplicationWindow;

    fn class_init(klass: &mut Self::Class) {
        klass.bind_template();

        klass.install_action("win.toggle-search", None, |win, _, _| {
            win.toggle_search();
        });
        klass.install_action("win.show-help", None, |win, _, _| win.show_help());
        klass.install_action("win.clear-history", None, |win, _, _| {
            win.confirm_clear();
        });
        klass.install_action(
            "win.copy-entry",
            Some(glib::VariantTy::STRING),
            |win, _, id| {
                if let Some(id) = id.and_then(|v| v.get::<String>()) {
                    win.copy_entry_by_id(&id);
                }
            },
        );
        klass.install_action(
            "win.delete-entry",
            Some(glib::VariantTy::STRING),
            |win, _, id| {
                if let Some(id) = id.and_then(|v| v.get::<String>()) {
                    win.delete_entry(&id);
                }
            },
        );
    }

    fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
        obj.init_template();
    }
}

impl ObjectImpl for HistoryWindow {
    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        self.search_bar.connect_entry(&*self.search_entry);
        obj.setup_list();
        obj.setup_keys();
    }

    fn signals() -> &'static [Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| vec![Signal::builder("history-cleared").build()])
    }
}

impl WidgetImpl for HistoryWindow {}

impl WindowImpl for HistoryWindow {
    /// Closing only hides the window; it is never destroyed.
    fn close_request(&self) -> glib::Propagation {
        self.obj().set_visible(false);
        glib::Propagation::Stop
    }
}

impl ApplicationWindowImpl for HistoryWindow {}
impl AdwApplicationWindowImpl for HistoryWindow {}
