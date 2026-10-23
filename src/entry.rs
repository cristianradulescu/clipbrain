//! `ClipEntry`: the GObject wrapper that list models hold for each history
//! entry, plus the row display helpers.

use std::path::{Path, PathBuf};

use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};

use crate::storage::{Entry, Kind};

/// Thumbnails fit in a square of this many pixels.
pub const THUMBNAIL_SIZE: u32 = 48;
/// Characters of a text entry's first line shown in its row.
const PREVIEW_CHARS: usize = 120;
/// Characters of a text entry shown in its row's tooltip.
const TOOLTIP_CHARS: usize = 500;

/// A decoded image thumbnail, or why there isn't one.
#[derive(Debug, Clone)]
pub enum Thumbnail {
    /// Downscaled image plus the original's dimensions.
    Loaded {
        texture: gdk::Texture,
        width: i32,
        height: i32,
    },
    /// The image file no longer exists.
    Missing,
    /// The image file couldn't be decoded.
    Failed,
}

mod imp {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::ClipEntry)]
    pub struct ClipEntry {
        /// Storage id (UUID).
        #[property(get, construct_only)]
        id: RefCell<String>,
        /// `"text"` or `"image"`.
        #[property(get, construct_only)]
        kind: RefCell<String>,
        /// Text content; empty for images.
        #[property(get, construct_only)]
        text: RefCell<String>,
        /// Absolute PNG path for images.
        #[property(get, construct_only)]
        image_path: RefCell<Option<String>>,
        /// ISO-8601 UTC timestamp.
        #[property(get, construct_only)]
        timestamp: RefCell<String>,
        /// Cached thumbnail, loaded on first bind.
        pub thumbnail: RefCell<Option<Thumbnail>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClipEntry {
        const NAME: &'static str = "ClipEntry";
        type Type = super::ClipEntry;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ClipEntry {}
}

glib::wrapper! {
    /// One clipboard history entry, as held by the window's list model.
    pub struct ClipEntry(ObjectSubclass<imp::ClipEntry>);
}

impl ClipEntry {
    /// Wraps a stored entry.
    pub fn new(entry: &Entry) -> Self {
        let kind = match entry.kind {
            Kind::Text => "text",
            Kind::Image => "image",
        };
        glib::Object::builder()
            .property("id", &entry.id)
            .property("kind", kind)
            .property("text", entry.text.as_deref().unwrap_or_default())
            .property(
                "image-path",
                entry
                    .image_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
            )
            .property("timestamp", &entry.timestamp)
            .build()
    }

    /// Whether this is an image entry.
    pub fn is_image(&self) -> bool {
        self.kind() == "image"
    }

    /// The thumbnail, if it has already been loaded.
    pub fn cached_thumbnail(&self) -> Option<Thumbnail> {
        self.imp().thumbnail.borrow().clone()
    }

    /// Loads (once) and returns the thumbnail. Decoding runs off-thread.
    pub async fn thumbnail(&self) -> Thumbnail {
        if let Some(thumbnail) = self.cached_thumbnail() {
            return thumbnail;
        }
        let path = self.image_path().map(PathBuf::from);
        let thumbnail = gio::spawn_blocking(move || load_thumbnail(path.as_deref()))
            .await
            .unwrap_or(Thumbnail::Failed);
        self.imp().thumbnail.replace(Some(thumbnail.clone()));
        thumbnail
    }

    /// Whether this entry matches a search query, like the Python filter:
    /// case-insensitive substring on text; images match substrings of
    /// "image".
    pub fn matches(&self, query: &str) -> bool {
        matches_query(self.is_image(), &self.text(), query)
    }
}

/// Decodes the full image at `path` off the main thread.
pub async fn load_texture(path: PathBuf) -> Option<gdk::Texture> {
    gio::spawn_blocking(move || gdk::Texture::from_filename(&path))
        .await
        .ok()?
        .inspect_err(|e| log::warn!("could not load image: {e}"))
        .ok()
}

fn matches_query(is_image: bool, text: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    if is_image {
        "image".contains(&query)
    } else {
        text.to_lowercase().contains(&query)
    }
}

/// First line of `text`, truncated with an ellipsis, for a row label.
pub fn text_preview(text: &str) -> String {
    let first_line = text.split('\n').next().unwrap_or_default();
    let mut chars = first_line.chars();
    let mut preview: String = chars.by_ref().take(PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

/// Start of `text`, for a row tooltip.
pub fn text_tooltip(text: &str) -> String {
    text.chars().take(TOOLTIP_CHARS).collect()
}

/// Decodes and downscales an image. Blocking; call off the main thread.
fn load_thumbnail(path: Option<&Path>) -> Thumbnail {
    let Some(path) = path.filter(|p| p.exists()) else {
        return Thumbnail::Missing;
    };
    let texture = match gdk::Texture::from_filename(path) {
        Ok(texture) => texture,
        Err(e) => {
            log::debug!("could not decode {}: {e}", path.display());
            return Thumbnail::Failed;
        }
    };
    let (width, height) = (texture.width(), texture.height());
    Thumbnail::Loaded {
        texture: downscale(&texture, THUMBNAIL_SIZE),
        width,
        height,
    }
}

/// Shrinks `texture` to fit in a `max`×`max` square, so the list doesn't
/// keep full-size screenshots in memory.
fn downscale(texture: &gdk::Texture, max: u32) -> gdk::Texture {
    let (width, height) = (texture.width() as u32, texture.height() as u32);
    if width <= max && height <= max {
        return texture.clone();
    }
    let scale = f64::from(max) / f64::from(width.max(height));
    let new_width = ((f64::from(width) * scale).round() as u32).max(1);
    let new_height = ((f64::from(height) * scale).round() as u32).max(1);

    let format = gdk::MemoryFormat::R8g8b8a8Premultiplied;
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(format);
    let (pixels, stride) = downloader.download_bytes();
    let scaled = box_filter(&pixels, stride, (width, height), (new_width, new_height));
    gdk::MemoryTexture::new(
        new_width as i32,
        new_height as i32,
        format,
        &glib::Bytes::from_owned(scaled),
        new_width as usize * 4,
    )
    .upcast()
}

/// Area-averaging downscale of 4-byte premultiplied pixels.
fn box_filter(src: &[u8], stride: usize, from: (u32, u32), to: (u32, u32)) -> Vec<u8> {
    let (src_w, src_h) = (from.0 as usize, from.1 as usize);
    let (dst_w, dst_h) = (to.0 as usize, to.1 as usize);
    let mut out = Vec::with_capacity(dst_w * dst_h * 4);
    for dy in 0..dst_h {
        let y0 = dy * src_h / dst_h;
        let y1 = ((dy + 1) * src_h / dst_h).max(y0 + 1);
        for dx in 0..dst_w {
            let x0 = dx * src_w / dst_w;
            let x1 = ((dx + 1) * src_w / dst_w).max(x0 + 1);
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let row = &src[y * stride..];
                for x in x0..x1 {
                    for (c, total) in sum.iter_mut().enumerate() {
                        *total += u32::from(row[x * 4 + c]);
                    }
                }
            }
            let count = ((y1 - y0) * (x1 - x0)) as u32;
            out.extend(sum.map(|total| (total / count) as u8));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_takes_first_line() {
        assert_eq!(text_preview("one\ntwo"), "one");
        assert_eq!(text_preview(""), "");
    }

    #[test]
    fn preview_truncates_long_lines_by_character() {
        let line = "é".repeat(PREVIEW_CHARS);
        assert_eq!(text_preview(&line), line);
        let long = "é".repeat(PREVIEW_CHARS + 1);
        assert_eq!(text_preview(&long), format!("{line}…"));
    }

    #[test]
    fn tooltip_is_capped() {
        let text = "x".repeat(TOOLTIP_CHARS + 10);
        assert_eq!(text_tooltip(&text).len(), TOOLTIP_CHARS);
    }

    #[test]
    fn query_matching_follows_python_rules() {
        assert!(matches_query(false, "Hello World", ""));
        assert!(matches_query(false, "Hello World", "  WORLD "));
        assert!(!matches_query(false, "Hello", "bye"));
        assert!(matches_query(true, "", "ima"));
        assert!(matches_query(true, "", "image"));
        assert!(!matches_query(true, "", "images"));
    }

    #[test]
    fn box_filter_averages_blocks() {
        // 2×2 → 1×1: average of the four pixels per channel.
        let src = [
            0, 0, 0, 255, 100, 100, 100, 255, //
            200, 200, 200, 255, 100, 100, 100, 255,
        ];
        assert_eq!(box_filter(&src, 8, (2, 2), (1, 1)), [100, 100, 100, 255]);
    }

    #[test]
    fn box_filter_respects_stride_padding() {
        // 1×2 image with 4 padding bytes per row.
        let src = [10, 20, 30, 40, 9, 9, 9, 9, 30, 40, 50, 60, 9, 9, 9, 9];
        assert_eq!(box_filter(&src, 8, (1, 2), (1, 1)), [20, 30, 40, 50]);
    }

    #[test]
    fn downscale_fits_the_box_and_keeps_aspect() {
        let pixels = glib::Bytes::from_owned(vec![255u8; 200 * 100 * 4]);
        let texture: gdk::Texture =
            gdk::MemoryTexture::new(200, 100, gdk::MemoryFormat::R8g8b8a8, &pixels, 800).upcast();
        let thumb = downscale(&texture, THUMBNAIL_SIZE);
        assert_eq!((thumb.width(), thumb.height()), (48, 24));
        let small = downscale(&thumb, THUMBNAIL_SIZE);
        assert_eq!((small.width(), small.height()), (48, 24));
    }

    #[test]
    fn missing_file_gives_missing_thumbnail() {
        assert!(matches!(
            load_thumbnail(Some(Path::new("/nonexistent/clipbrain.png"))),
            Thumbnail::Missing
        ));
        assert!(matches!(load_thumbnail(None), Thumbnail::Missing));
    }

    #[test]
    fn fixture_png_loads_as_thumbnail() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_image.png");
        let Thumbnail::Loaded { width, height, .. } = load_thumbnail(Some(&path)) else {
            panic!("fixture should decode");
        };
        assert_eq!((width, height), (2, 2));
    }
}
