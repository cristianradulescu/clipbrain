//! Persistent clipboard history, on-disk compatible with the Python version.
//!
//! Layout under the data directory (`$XDG_DATA_HOME/clipbrain`, falling back
//! to `~/.local/share/clipbrain`):
//!
//! - `history.json`: JSON array of [`Entry`], newest first, at most
//!   [`MAX_ENTRIES`] long, pretty-printed with a 2-space indent.
//! - `images/<id>.png`: one PNG per image entry, referenced by absolute path.
//!
//! Text entries are deduplicated by exact content and image entries by the
//! SHA-256 of their PNG bytes. Re-adding an existing item moves it to the
//! front and keeps its original id and timestamp.
//!
//! This module has no GTK dependency. Every call does blocking file I/O, so
//! the GUI must call it off the main thread.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Maximum number of entries kept; the oldest are evicted beyond this.
pub const MAX_ENTRIES: usize = 200;

const INDEX_FILE: &str = "history.json";
const IMAGES_DIR: &str = "images";

/// Errors returned by [`Store`] operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The user's home directory could not be determined.
    #[error("could not determine the user data directory")]
    NoDataDir,
    /// A filesystem operation failed.
    #[error("{op} {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The history could not be serialised.
    #[error("failed to encode history: {0}")]
    Encode(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, StorageError>;

fn io_err(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> StorageError {
    let path = path.to_path_buf();
    move |source| StorageError::Io { op, path, source }
}

/// The type of content an entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Image,
}

/// One history entry, serialised exactly as the Python version wrote it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Random UUIDv4 string.
    pub id: String,
    pub kind: Kind,
    /// UTC timestamp in Python `datetime.isoformat()` form.
    pub timestamp: String,
    /// Content of a text entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Absolute path of an image entry's PNG file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_path: Option<PathBuf>,
    /// SHA-256 hex digest of an image entry's PNG bytes. Missing on entries
    /// written by old versions, which therefore never deduplicate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Unknown keys, preserved so a load/save round trip loses nothing.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The clipboard history: an in-memory copy of `history.json` that is
/// written back atomically after every change.
#[derive(Debug)]
pub struct Store {
    index_file: PathBuf,
    images_dir: PathBuf,
    entries: Vec<Entry>,
}

impl Store {
    /// Opens the history in the default data directory.
    pub fn open_default() -> Result<Self> {
        let base = directories::BaseDirs::new().ok_or(StorageError::NoDataDir)?;
        Self::open(&base.data_dir().join("clipbrain"))
    }

    /// Opens (creating if needed) the history stored in `dir`.
    ///
    /// A missing index yields an empty history. An index that is not valid
    /// JSON is renamed to `history.json.corrupt-<timestamp>` and an empty
    /// history is started, so the next save cannot destroy it.
    pub fn open(dir: &Path) -> Result<Self> {
        let dir = std::path::absolute(dir).map_err(io_err("resolve", dir))?;
        let images_dir = dir.join(IMAGES_DIR);
        fs::create_dir_all(&images_dir).map_err(io_err("create", &images_dir))?;
        let index_file = dir.join(INDEX_FILE);
        let entries = load_index(&index_file)?;
        Ok(Self {
            index_file,
            images_dir,
            entries,
        })
    }

    /// All entries, newest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Adds a text entry, or moves an identical existing one to the front.
    pub fn add_text(&mut self, text: &str) -> Result<Entry> {
        let existing = self
            .entries
            .iter()
            .position(|e| e.kind == Kind::Text && e.text.as_deref() == Some(text));
        if let Some(pos) = existing {
            return self.move_to_front(pos);
        }

        let entry = Entry {
            id: new_id(),
            kind: Kind::Text,
            timestamp: format_timestamp(Utc::now()),
            text: Some(text.to_owned()),
            image_path: None,
            content_hash: None,
            extra: Default::default(),
        };
        self.insert_new(entry)
    }

    /// Adds an image entry from PNG bytes, or moves an existing entry with
    /// the same content hash to the front.
    pub fn add_image(&mut self, png: &[u8]) -> Result<Entry> {
        let hash = sha256_hex(png);
        let existing = self
            .entries
            .iter()
            .position(|e| e.kind == Kind::Image && e.content_hash.as_deref() == Some(&hash));
        if let Some(pos) = existing {
            return self.move_to_front(pos);
        }

        let id = new_id();
        let image_path = self.images_dir.join(format!("{id}.png"));
        write_atomic(&image_path, png)?;
        let entry = Entry {
            id,
            kind: Kind::Image,
            timestamp: format_timestamp(Utc::now()),
            text: None,
            image_path: Some(image_path),
            content_hash: Some(hash),
            extra: Default::default(),
        };
        self.insert_new(entry)
    }

    /// Moves the entry with `id` to the front. Returns `false` if not found.
    pub fn promote(&mut self, id: &str) -> Result<bool> {
        match self.entries.iter().position(|e| e.id == id) {
            Some(pos) => self.move_to_front(pos).map(|_| true),
            None => Ok(false),
        }
    }

    /// Removes the entry with `id` and its image file. Returns `false` if
    /// not found.
    pub fn delete(&mut self, id: &str) -> Result<bool> {
        let Some(pos) = self.entries.iter().position(|e| e.id == id) else {
            return Ok(false);
        };
        let entry = self.entries.remove(pos);
        remove_image(&entry);
        self.save()?;
        Ok(true)
    }

    /// Deletes every entry and every file in the images directory.
    pub fn clear(&mut self) -> Result<()> {
        match fs::remove_dir_all(&self.images_dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(io_err("remove", &self.images_dir)(e));
            }
            _ => {}
        }
        fs::create_dir_all(&self.images_dir).map_err(io_err("create", &self.images_dir))?;
        self.entries.clear();
        self.save()
    }

    /// The content hash of the newest entry, as the clipboard monitor
    /// computes it, so a restart doesn't re-store the current clipboard.
    pub fn seed_hash(&self) -> Option<String> {
        let top = self.entries.first()?;
        match top.kind {
            Kind::Text => top
                .text
                .as_deref()
                .filter(|t| !t.is_empty())
                .map(|t| sha256_hex(t.as_bytes())),
            Kind::Image => top.content_hash.clone(),
        }
    }

    fn move_to_front(&mut self, pos: usize) -> Result<Entry> {
        let entry = self.entries.remove(pos);
        self.entries.insert(0, entry.clone());
        self.save()?;
        Ok(entry)
    }

    fn insert_new(&mut self, entry: Entry) -> Result<Entry> {
        self.entries.insert(0, entry.clone());
        self.prune();
        self.save()?;
        Ok(entry)
    }

    /// Trims to [`MAX_ENTRIES`], deleting evicted entries' image files.
    fn prune(&mut self) {
        while self.entries.len() > MAX_ENTRIES {
            if let Some(oldest) = self.entries.pop() {
                remove_image(&oldest);
            }
        }
    }

    fn save(&self) -> Result<()> {
        let json = serde_json::to_vec_pretty(&self.entries)?;
        write_atomic(&self.index_file, &json)
    }
}

/// Lowercase hex SHA-256 of `data`, matching Python's `hexdigest()`.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Formats like Python's `datetime.now(timezone.utc).isoformat()`, which
/// omits the fractional part when the microseconds are zero.
fn format_timestamp(t: DateTime<Utc>) -> String {
    if t.timestamp_subsec_micros() == 0 {
        t.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        t.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}

fn load_index(path: &Path) -> Result<Vec<Entry>> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err("read", path)(e)),
    };
    match serde_json::from_slice(&data) {
        Ok(entries) => Ok(entries),
        Err(err) => {
            let stamp = Utc::now().format("%Y%m%dT%H%M%S");
            let backup = path.with_file_name(format!("{INDEX_FILE}.corrupt-{stamp}"));
            log::warn!(
                "{} is not valid history ({err}); moving it to {} and starting empty",
                path.display(),
                backup.display()
            );
            fs::rename(path, &backup).map_err(io_err("rename", path))?;
            Ok(Vec::new())
        }
    }
}

/// Writes `data` to a sibling temp file, syncs it, then renames it over
/// `path`, so readers never see a partially written file.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let mut file = fs::File::create(&tmp).map_err(io_err("create", &tmp))?;
    file.write_all(data).map_err(io_err("write", &tmp))?;
    file.sync_all().map_err(io_err("sync", &tmp))?;
    drop(file);
    fs::rename(&tmp, path).map_err(io_err("rename", &tmp))
}

/// Deletes an image entry's file, ignoring errors like the Python version.
fn remove_image(entry: &Entry) {
    if entry.kind != Kind::Image {
        return;
    }
    if let Some(path) = &entry.image_path
        && let Err(e) = fs::remove_file(path)
    {
        log::debug!("could not remove {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    const FIXTURE: &str = include_str!("../tests/fixtures/python_history.json");
    const FIXTURE_PNG: &[u8] = include_bytes!("../tests/fixtures/python_image.png");

    fn empty_store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn fixture_store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(INDEX_FILE), FIXTURE).unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn png(n: u32) -> Vec<u8> {
        // Content only needs to be unique; storage never decodes it.
        format!("fake png {n}").into_bytes()
    }

    fn image_files(dir: &TempDir) -> usize {
        fs::read_dir(dir.path().join(IMAGES_DIR)).unwrap().count()
    }

    #[test]
    fn missing_index_is_empty_and_creates_dirs() {
        let (dir, store) = empty_store();
        assert!(store.entries().is_empty());
        assert!(dir.path().join(IMAGES_DIR).is_dir());
    }

    #[test]
    fn loads_python_fixture() {
        let (_dir, store) = fixture_store();
        let e = store.entries();
        assert_eq!(e.len(), 6);
        assert_eq!(e[0].text.as_deref(), Some("multi\nline\nentry"));
        assert_eq!(
            e[1].text.as_deref(),
            Some("unicode: café ünïcödé 日本語 🦀")
        );
        assert_eq!(
            e[2].text.as_deref(),
            Some("tricky: \"quotes\" \\ back\tslash\nsecond line \u{1} ctrl")
        );
        assert_eq!(e[3].kind, Kind::Image);
        assert_eq!(
            e[3].content_hash.as_deref(),
            Some(&*sha256_hex(FIXTURE_PNG))
        );
        assert_eq!(e[4].timestamp, "2026-01-02T03:04:05+00:00");
        assert_eq!(e[5].kind, Kind::Image);
        assert_eq!(e[5].content_hash, None);
    }

    #[test]
    fn python_fixture_round_trips_byte_for_byte() {
        let (dir, mut store) = fixture_store();
        let top = store.entries()[0].id.clone();
        assert!(store.promote(&top).unwrap()); // forces a save, order unchanged
        let saved = fs::read_to_string(dir.path().join(INDEX_FILE)).unwrap();
        assert_eq!(saved, FIXTURE);
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let dir = TempDir::new().unwrap();
        let json =
            r#"[{"id": "a", "kind": "text", "timestamp": "t", "text": "x", "pinned": true}]"#;
        fs::write(dir.path().join(INDEX_FILE), json).unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.add_text("y").unwrap();
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(
            reopened.entries()[1].extra["pinned"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn add_text_prepends_and_persists() {
        let (dir, mut store) = empty_store();
        let a = store.add_text("a").unwrap();
        let b = store.add_text("b").unwrap();
        assert_eq!(a.kind, Kind::Text);
        assert_ne!(a.id, b.id);
        let reopened = Store::open(dir.path()).unwrap();
        let texts: Vec<_> = reopened.entries().iter().map(|e| e.text.clone()).collect();
        assert_eq!(texts, [Some("b".into()), Some("a".into())]);
    }

    #[test]
    fn duplicate_text_moves_to_front_keeping_id_and_timestamp() {
        let (_dir, mut store) = fixture_store();
        let before = store.entries()[4].clone();
        let returned = store.add_text("oldest plain text").unwrap();
        assert_eq!(returned, before);
        assert_eq!(store.entries()[0], before);
        assert_eq!(store.entries().len(), 6);
    }

    #[test]
    fn text_dedup_is_exact() {
        let (_dir, mut store) = empty_store();
        store.add_text("abc").unwrap();
        store.add_text("abc ").unwrap();
        store.add_text("ABC").unwrap();
        assert_eq!(store.entries().len(), 3);
    }

    #[test]
    fn add_image_writes_png_with_hash() {
        let (dir, mut store) = empty_store();
        let entry = store.add_image(&png(1)).unwrap();
        let path = entry.image_path.clone().unwrap();
        assert!(path.is_absolute());
        assert_eq!(
            path,
            dir.path()
                .join(IMAGES_DIR)
                .join(format!("{}.png", entry.id))
        );
        assert_eq!(fs::read(&path).unwrap(), png(1));
        assert_eq!(entry.content_hash, Some(sha256_hex(&png(1))));
    }

    #[test]
    fn duplicate_image_moves_to_front_without_new_file() {
        let (dir, mut store) = empty_store();
        let first = store.add_image(&png(1)).unwrap();
        store.add_text("between").unwrap();
        let again = store.add_image(&png(1)).unwrap();
        assert_eq!(again, first);
        assert_eq!(store.entries()[0].id, first.id);
        assert_eq!(store.entries().len(), 2);
        assert_eq!(image_files(&dir), 1);
    }

    #[test]
    fn image_dedups_against_python_content_hash() {
        let (_dir, mut store) = fixture_store();
        let python_image = store.entries()[3].clone();
        let returned = store.add_image(FIXTURE_PNG).unwrap();
        assert_eq!(returned.id, python_image.id);
        assert_eq!(store.entries()[0].id, python_image.id);
        assert_eq!(store.entries().len(), 6);
    }

    #[test]
    fn cap_evicts_oldest_and_deletes_their_pngs() {
        let (dir, mut store) = empty_store();
        let oldest = store.add_image(&png(0)).unwrap();
        for i in 1..MAX_ENTRIES {
            store.add_text(&format!("text {i}")).unwrap();
        }
        assert_eq!(store.entries().len(), MAX_ENTRIES);
        assert_eq!(image_files(&dir), 1);

        store.add_text("one too many").unwrap();
        assert_eq!(store.entries().len(), MAX_ENTRIES);
        assert!(store.entries().iter().all(|e| e.id != oldest.id));
        assert!(!oldest.image_path.unwrap().exists());
        assert_eq!(image_files(&dir), 0);
        assert_eq!(
            Store::open(dir.path()).unwrap().entries().len(),
            MAX_ENTRIES
        );
    }

    #[test]
    fn promote_moves_entry_to_front() {
        let (_dir, mut store) = fixture_store();
        let id = store.entries()[3].id.clone();
        assert!(store.promote(&id).unwrap());
        assert_eq!(store.entries()[0].id, id);
        assert!(!store.promote("no-such-id").unwrap());
    }

    #[test]
    fn delete_removes_entry_and_png() {
        let (dir, mut store) = empty_store();
        let img = store.add_image(&png(1)).unwrap();
        store.add_text("keep").unwrap();
        assert!(store.delete(&img.id).unwrap());
        assert!(!img.image_path.unwrap().exists());
        assert!(!store.delete(&img.id).unwrap());
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.entries().len(), 1);
    }

    #[test]
    fn delete_tolerates_missing_png() {
        let (_dir, mut store) = fixture_store();
        let id = store.entries()[5].id.clone(); // its file never existed
        assert!(store.delete(&id).unwrap());
    }

    #[test]
    fn clear_removes_everything() {
        let (dir, mut store) = empty_store();
        store.add_image(&png(1)).unwrap();
        store.add_text("x").unwrap();
        fs::write(dir.path().join(IMAGES_DIR).join("stray.png"), b"x").unwrap();
        store.clear().unwrap();
        assert!(store.entries().is_empty());
        assert_eq!(image_files(&dir), 0);
        let saved = fs::read_to_string(dir.path().join(INDEX_FILE)).unwrap();
        assert_eq!(saved, "[]");
    }

    #[test]
    fn writes_leave_no_temp_files() {
        let (dir, mut store) = empty_store();
        store.add_text("x").unwrap();
        store.add_image(&png(1)).unwrap();
        let tmp_files = walk(dir.path())
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .count();
        assert_eq!(tmp_files, 0);
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    #[test]
    fn corrupt_index_is_backed_up_not_overwritten() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(INDEX_FILE), "{ not json").unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        assert!(store.entries().is_empty());
        store.add_text("fresh").unwrap();

        let backups: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("history.json.corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1);
        let backup = fs::read_to_string(dir.path().join(&backups[0])).unwrap();
        assert_eq!(backup, "{ not json");
    }

    #[test]
    fn timestamp_matches_python_isoformat() {
        let t = Utc.with_ymd_and_hms(2026, 10, 23, 7, 58, 47).unwrap();
        assert_eq!(format_timestamp(t), "2026-10-23T07:58:47+00:00");
        let t = t + chrono::Duration::microseconds(966_762);
        assert_eq!(format_timestamp(t), "2026-10-23T07:58:47.966762+00:00");
        let t = t + chrono::Duration::nanoseconds(999); // sub-µs is truncated
        assert_eq!(format_timestamp(t), "2026-10-23T07:58:47.966762+00:00");
        let t =
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + chrono::Duration::microseconds(1);
        assert_eq!(format_timestamp(t), "2026-01-01T00:00:00.000001+00:00");
    }

    #[test]
    fn new_entries_use_uuid_v4() {
        let (_dir, mut store) = empty_store();
        let e = store.add_text("x").unwrap();
        let id = uuid::Uuid::parse_str(&e.id).unwrap();
        assert_eq!(id.get_version_num(), 4);
        assert_eq!(e.id, id.hyphenated().to_string());
    }

    #[test]
    fn seed_hash_matches_monitor_hashing() {
        let (_dir, mut store) = empty_store();
        assert_eq!(store.seed_hash(), None);
        store.add_text("hello").unwrap();
        assert_eq!(
            store.seed_hash().as_deref(),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        let img = store.add_image(&png(1)).unwrap();
        assert_eq!(store.seed_hash(), img.content_hash);
    }

    #[test]
    fn sha256_hex_matches_python_hexdigest() {
        // hashlib.sha256(b"").hexdigest()
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
