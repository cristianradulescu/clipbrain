//! Clipboard watcher: a port of the Python `ClipboardMonitor`.
//!
//! Two capture paths feed one dedup step:
//!
//! - The clipboard's `changed` signal. Mutter coalesces it, so rapid copies
//!   from one app fire it only once.
//! - A 500 ms poll that reads the clipboard and catches whatever the signal
//!   missed. Don't remove it.
//!
//! Every `changed` event bumps a generation counter and cancels in-flight
//! reads; results from an older generation are dropped. A capture is only
//! delivered if its SHA-256 differs from the last delivered one, which
//! suppresses the spurious `changed` signals GNOME fires on focus changes.
//!
//! Images are checked before text, because apps that copy an image often
//! also offer `text/plain`. Hashing and PNG encoding run off the main thread.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk::{gdk, gio, glib, prelude::*};

use crate::storage::sha256_hex;

/// Image MIME types we capture, in order of preference.
const IMAGE_MIMES: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/bmp",
    "image/tiff",
    "image/webp",
];
const TEXT_MIMES: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];

/// How often the clipboard is polled.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// A poll read still running after this long is cancelled, so a source app
/// that never finishes writing can't stall polling forever.
const POLL_TIMEOUT: Duration = Duration::from_secs(3);

/// A boxed, non-`Send` future, as produced by GIO async calls.
pub type LocalBoxFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// Read access to a clipboard. Dropping a returned future cancels the read.
pub trait ClipboardSource: 'static {
    /// MIME types currently offered.
    fn mime_types(&self) -> Vec<String>;
    /// Whether the current content was set by this process.
    fn is_local(&self) -> bool;
    /// Reads the content as text.
    fn read_text(&self) -> LocalBoxFuture<Option<String>>;
    /// Reads the content as a decoded image.
    fn read_texture(&self) -> LocalBoxFuture<Option<gdk::Texture>>;
    /// Reads the raw bytes offered for `mime`.
    fn read_mime(&self, mime: &str) -> LocalBoxFuture<Option<glib::Bytes>>;
    /// Calls `f` whenever the clipboard content changes.
    fn connect_changed(&self, f: Box<dyn Fn()>);
}

/// [`ClipboardSource`] backed by a GDK clipboard.
pub struct GdkClipboardSource {
    clipboard: gdk::Clipboard,
}

impl GdkClipboardSource {
    /// Wraps `clipboard`.
    pub fn new(clipboard: gdk::Clipboard) -> Self {
        Self { clipboard }
    }
}

impl ClipboardSource for GdkClipboardSource {
    fn mime_types(&self) -> Vec<String> {
        self.clipboard
            .formats()
            .mime_types()
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn is_local(&self) -> bool {
        self.clipboard.is_local()
    }

    fn read_text(&self) -> LocalBoxFuture<Option<String>> {
        let read = self.clipboard.read_text_future();
        Box::pin(async move {
            read.await
                .inspect_err(|e| log::debug!("text read failed: {e}"))
                .ok()
                .flatten()
                .map(String::from)
        })
    }

    fn read_texture(&self) -> LocalBoxFuture<Option<gdk::Texture>> {
        let read = self.clipboard.read_texture_future();
        Box::pin(async move {
            read.await
                .inspect_err(|e| log::debug!("texture read failed: {e}"))
                .ok()
                .flatten()
        })
    }

    fn read_mime(&self, mime: &str) -> LocalBoxFuture<Option<glib::Bytes>> {
        let read = self.clipboard.read_future(&[mime], glib::Priority::DEFAULT);
        Box::pin(async move {
            let (stream, _mime) = read
                .await
                .inspect_err(|e| log::debug!("stream read failed: {e}"))
                .ok()?;
            let out = gio::MemoryOutputStream::new_resizable();
            let flags = gio::OutputStreamSpliceFlags::CLOSE_SOURCE
                | gio::OutputStreamSpliceFlags::CLOSE_TARGET;
            out.splice_future(&stream, flags, glib::Priority::DEFAULT)
                .await
                .inspect_err(|e| log::debug!("stream copy failed: {e}"))
                .ok()?;
            Some(out.steal_as_bytes())
        })
    }

    fn connect_changed(&self, f: Box<dyn Fn()>) {
        self.clipboard.connect_changed(move |_| f());
    }
}

/// New clipboard content, ready to be stored.
#[derive(Debug, Clone, PartialEq)]
pub enum Capture {
    Text(String),
    /// PNG-encoded image.
    Image(glib::Bytes),
}

/// Watches a [`ClipboardSource`] and reports each new, distinct content.
pub struct Monitor<S: ClipboardSource> {
    inner: Rc<Inner<S>>,
}

impl<S: ClipboardSource> Monitor<S> {
    /// Creates a monitor. `seed_hash` is the hash of the newest stored entry
    /// (see `Store::seed_hash`), so a restart doesn't re-store the current
    /// clipboard. `on_capture` runs on the main thread.
    pub fn new(
        source: S,
        seed_hash: Option<String>,
        on_capture: impl Fn(Capture) + 'static,
    ) -> Self {
        let inner = Rc::new(Inner {
            source,
            on_capture: Box::new(on_capture),
            state: RefCell::new(State {
                last_hash: seed_hash,
                ..State::default()
            }),
            changed_task: RefCell::default(),
            poll_task: RefCell::default(),
            poll_seq: Cell::new(0),
            poll_timeout: Cell::new(POLL_TIMEOUT),
        });
        Self { inner }
    }

    /// Connects the `changed` signal and starts the poll timer.
    pub fn start(&self) {
        let weak = Rc::downgrade(&self.inner);
        self.inner.source.connect_changed(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.on_changed();
            }
        }));

        let weak: Weak<Inner<S>> = Rc::downgrade(&self.inner);
        glib::timeout_add_local(POLL_INTERVAL, move || match weak.upgrade() {
            Some(inner) => {
                inner.poll();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    /// Forgets the last delivered hash. Call after clearing history, or
    /// re-copying the current clipboard content is silently dropped.
    pub fn reset_last_hash(&self) {
        self.inner.state.borrow_mut().last_hash = None;
    }
}

/// Dedup bookkeeping shared by both capture paths.
#[derive(Debug, Default)]
struct State {
    /// Bumped on every `changed` event; older reads are stale.
    generation: u64,
    /// Hash of the last delivered content.
    last_hash: Option<String>,
    /// Hash of the raw image bytes last seen by the poll, so an image sitting
    /// on the clipboard isn't decoded and re-encoded every tick.
    poll_raw_image_hash: Option<String>,
}

impl State {
    /// Records `hash` as delivered unless it is stale or a repeat.
    fn accept(&mut self, generation: u64, hash: String) -> bool {
        if generation != self.generation || self.last_hash.as_ref() == Some(&hash) {
            return false;
        }
        self.last_hash = Some(hash);
        true
    }
}

struct PollTask {
    seq: u64,
    started: Instant,
    handle: glib::JoinHandle<()>,
}

struct Inner<S> {
    source: S,
    on_capture: Box<dyn Fn(Capture)>,
    state: RefCell<State>,
    changed_task: RefCell<Option<glib::JoinHandle<()>>>,
    poll_task: RefCell<Option<PollTask>>,
    poll_seq: Cell<u64>,
    poll_timeout: Cell<Duration>,
}

impl<S: ClipboardSource> Inner<S> {
    fn generation(&self) -> u64 {
        self.state.borrow().generation
    }

    fn on_changed(self: &Rc<Self>) {
        // Cancel in-flight reads so the compositor releases the previous
        // offer immediately; it may withhold `changed` signals until then.
        if let Some(task) = self.changed_task.take() {
            task.abort();
        }
        if let Some(task) = self.poll_task.take() {
            task.handle.abort();
        }
        let generation = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.generation
        };
        if self.source.is_local() {
            log::debug!("ignoring clipboard change made by clipbrain");
            return;
        }

        let mimes = self.source.mime_types();
        let this = Rc::clone(self);
        let handle = glib::spawn_future_local(async move {
            this.read_changed(&mimes, generation).await;
        });
        self.changed_task.replace(Some(handle));
    }

    async fn read_changed(&self, mimes: &[String], generation: u64) {
        let Some(mime) = find_mime(mimes, &IMAGE_MIMES) else {
            if let Some(text) = self.source.read_text().await {
                self.deliver_text(text, generation).await;
            }
            return;
        };
        match self.source.read_texture().await {
            Some(texture) => {
                let encoded = off_thread(move || {
                    let png = texture.save_to_png_bytes();
                    let hash = sha256_hex(&png);
                    (png, hash)
                });
                if let Some((png, hash)) = encoded.await {
                    self.deliver_image(png, hash, generation);
                }
            }
            None => {
                // Fall back to the raw stream when GDK can't hand us a texture.
                if let Some(raw) = self.source.read_mime(mime).await {
                    self.decode_and_deliver(raw, generation).await;
                }
            }
        }
    }

    fn poll(self: &Rc<Self>) {
        if let Some(task) = self.poll_task.borrow().as_ref()
            && task.started.elapsed() < self.poll_timeout.get()
        {
            return;
        }
        if let Some(stuck) = self.poll_task.take() {
            log::debug!(
                "cancelling clipboard poll stuck for {:?}",
                stuck.started.elapsed()
            );
            stuck.handle.abort();
        }
        if self.source.is_local() {
            return;
        }

        let mimes = self.source.mime_types();
        let image_mime = find_mime(&mimes, &IMAGE_MIMES);
        let has_text = find_mime(&mimes, &TEXT_MIMES).is_some();
        if image_mime.is_none() && !has_text {
            return;
        }

        // The poll doesn't bump the generation (that would discard in-flight
        // `changed` reads); it drops its own result if one arrived meanwhile.
        let generation = self.generation();
        let seq = self.poll_seq.get() + 1;
        self.poll_seq.set(seq);
        let this = Rc::clone(self);
        let handle = glib::spawn_future_local(async move {
            this.read_polled(image_mime, has_text, generation).await;
            let mut task = this.poll_task.borrow_mut();
            if task.as_ref().is_some_and(|t| t.seq == seq) {
                *task = None;
            }
        });
        self.poll_task.replace(Some(PollTask {
            seq,
            started: Instant::now(),
            handle,
        }));
    }

    async fn read_polled(&self, image_mime: Option<&'static str>, has_text: bool, generation: u64) {
        if let Some(mime) = image_mime {
            let raw = self.source.read_mime(mime).await.filter(|b| !b.is_empty());
            if let Some(raw) = raw {
                let Some((raw, raw_hash)) = off_thread(move || {
                    let hash = sha256_hex(&raw);
                    (raw, hash)
                })
                .await
                else {
                    return;
                };
                {
                    let mut state = self.state.borrow_mut();
                    if generation != state.generation
                        || state.poll_raw_image_hash.as_ref() == Some(&raw_hash)
                    {
                        return;
                    }
                    state.poll_raw_image_hash = Some(raw_hash);
                }
                self.decode_and_deliver(raw, generation).await;
                return;
            }
            // The format list only refreshes while focused, so it may be
            // stale: the clipboard may hold text by now.
            if !has_text {
                return;
            }
        }
        if let Some(text) = self.source.read_text().await {
            self.deliver_text(text, generation).await;
        }
    }

    async fn decode_and_deliver(&self, raw: glib::Bytes, generation: u64) {
        let encoded = off_thread(move || match gdk::Texture::from_bytes(&raw) {
            Ok(texture) => {
                let png = texture.save_to_png_bytes();
                let hash = sha256_hex(&png);
                Some((png, hash))
            }
            Err(e) => {
                log::debug!("could not decode clipboard image: {e}");
                None
            }
        });
        if let Some(Some((png, hash))) = encoded.await {
            self.deliver_image(png, hash, generation);
        }
    }

    async fn deliver_text(&self, text: String, generation: u64) {
        if text.trim().is_empty() {
            return;
        }
        let hashed = off_thread(move || {
            let hash = sha256_hex(text.as_bytes());
            (text, hash)
        });
        if let Some((text, hash)) = hashed.await
            && self.state.borrow_mut().accept(generation, hash)
        {
            log::debug!("captured text ({} bytes)", text.len());
            (self.on_capture)(Capture::Text(text));
        }
    }

    fn deliver_image(&self, png: glib::Bytes, hash: String, generation: u64) {
        if self.state.borrow_mut().accept(generation, hash) {
            log::debug!("captured image ({} bytes PNG)", png.len());
            (self.on_capture)(Capture::Image(png));
        }
    }
}

fn find_mime(offered: &[String], wanted: &[&'static str]) -> Option<&'static str> {
    wanted
        .iter()
        .copied()
        .find(|w| offered.iter().any(|o| o == w))
}

/// Runs `f` on GIO's blocking thread pool. Returns `None` if it panicked.
async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    gio::spawn_blocking(f)
        .await
        .inspect_err(|_| log::error!("clipboard worker thread panicked"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripted clipboard. Reads resolve immediately unless `gate` is set,
    /// in which case they wait for a message on it.
    #[derive(Default)]
    struct Fake {
        mimes: Vec<String>,
        text: Option<String>,
        texture: Option<gdk::Texture>,
        raw: Option<glib::Bytes>,
        local: bool,
        gate: Option<async_channel::Receiver<()>>,
        reads: u32,
        changed: Option<Rc<dyn Fn()>>,
    }

    #[derive(Clone, Default)]
    struct FakeSource(Rc<RefCell<Fake>>);

    impl FakeSource {
        fn set_text(&self, text: &str) {
            let mut f = self.0.borrow_mut();
            f.mimes = vec!["text/plain;charset=utf-8".into()];
            f.text = Some(text.into());
            f.texture = None;
            f.raw = None;
        }

        fn set_image(&self, png: &[u8]) {
            let mut f = self.0.borrow_mut();
            f.mimes = vec!["image/png".into()];
            f.text = None;
            f.raw = Some(glib::Bytes::from(png));
            f.texture = gdk::Texture::from_bytes(&glib::Bytes::from(png)).ok();
        }

        fn emit_changed(&self) {
            let changed = self.0.borrow().changed.clone();
            if let Some(changed) = changed {
                changed();
            }
        }

        fn reads(&self) -> u32 {
            self.0.borrow().reads
        }

        fn read<T: 'static>(&self, value: Option<T>) -> LocalBoxFuture<Option<T>> {
            let gate = {
                let mut f = self.0.borrow_mut();
                f.reads += 1;
                f.gate.clone()
            };
            Box::pin(async move {
                if let Some(gate) = gate {
                    gate.recv().await.ok()?;
                }
                value
            })
        }
    }

    impl ClipboardSource for FakeSource {
        fn mime_types(&self) -> Vec<String> {
            self.0.borrow().mimes.clone()
        }
        fn is_local(&self) -> bool {
            self.0.borrow().local
        }
        fn read_text(&self) -> LocalBoxFuture<Option<String>> {
            let value = self.0.borrow().text.clone();
            self.read(value)
        }
        fn read_texture(&self) -> LocalBoxFuture<Option<gdk::Texture>> {
            let value = self.0.borrow().texture.clone();
            self.read(value)
        }
        fn read_mime(&self, _mime: &str) -> LocalBoxFuture<Option<glib::Bytes>> {
            let value = self.0.borrow().raw.clone();
            self.read(value)
        }
        fn connect_changed(&self, f: Box<dyn Fn()>) {
            self.0.borrow_mut().changed = Some(Rc::from(f));
        }
    }

    struct Harness {
        source: FakeSource,
        monitor: Monitor<FakeSource>,
        captures: async_channel::Receiver<Capture>,
    }

    impl Harness {
        fn new(seed: Option<String>) -> Self {
            let source = FakeSource::default();
            let (tx, captures) = async_channel::unbounded();
            let monitor = Monitor::new(source.clone(), seed, move |c| {
                tx.try_send(c).unwrap();
            });
            // Hook up `changed` without the real timer; tests poll by hand.
            let weak = Rc::downgrade(&monitor.inner);
            source.connect_changed(Box::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.on_changed();
                }
            }));
            Self {
                source,
                monitor,
                captures,
            }
        }

        fn poll(&self) {
            self.monitor.inner.poll();
        }

        /// Waits for in-flight tasks (including thread-pool work) to finish
        /// and returns everything captured so far.
        async fn drain(&self) -> Vec<Capture> {
            glib::timeout_future(Duration::from_millis(150)).await;
            let mut out = Vec::new();
            while let Ok(c) = self.captures.try_recv() {
                out.push(c);
            }
            out
        }
    }

    fn run(test: impl AsyncFnOnce()) {
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| ctx.block_on(test())).unwrap();
    }

    fn text(s: &str) -> Capture {
        Capture::Text(s.into())
    }

    fn sample_png(rgba: [u8; 4]) -> glib::Bytes {
        let pixels = glib::Bytes::from_owned(rgba.repeat(4));
        gdk::MemoryTexture::new(2, 2, gdk::MemoryFormat::R8g8b8a8, &pixels, 8).save_to_png_bytes()
    }

    #[test]
    fn state_accepts_new_hashes_once() {
        let mut s = State::default();
        assert!(s.accept(0, "a".into()));
        assert!(!s.accept(0, "a".into()));
        assert!(s.accept(0, "b".into()));
        assert!(s.accept(0, "a".into()));
    }

    #[test]
    fn state_rejects_stale_generation() {
        let mut s = State {
            generation: 2,
            ..State::default()
        };
        assert!(!s.accept(1, "a".into()));
        assert_eq!(s.last_hash, None);
        assert!(s.accept(2, "a".into()));
    }

    #[test]
    fn changed_signal_captures_text_once() {
        run(async || {
            let h = Harness::new(None);
            h.source.set_text("hello");
            h.source.emit_changed();
            assert_eq!(h.drain().await, [text("hello")]);
            // Spurious `changed` on focus change: same content, no capture.
            h.source.emit_changed();
            h.poll();
            assert_eq!(h.drain().await, []);
        });
    }

    #[test]
    fn whitespace_only_text_is_ignored() {
        run(async || {
            let h = Harness::new(None);
            h.source.set_text(" \n\t ");
            h.source.emit_changed();
            h.poll();
            assert_eq!(h.drain().await, []);
        });
    }

    #[test]
    fn seed_hash_suppresses_current_content() {
        run(async || {
            let h = Harness::new(Some(sha256_hex(b"already stored")));
            h.source.set_text("already stored");
            h.poll();
            assert_eq!(h.drain().await, []);
        });
    }

    #[test]
    fn reset_last_hash_allows_recapture() {
        run(async || {
            let h = Harness::new(None);
            h.source.set_text("x");
            h.poll();
            assert_eq!(h.drain().await, [text("x")]);
            h.monitor.reset_last_hash();
            h.poll();
            assert_eq!(h.drain().await, [text("x")]);
        });
    }

    #[test]
    fn poll_catches_copies_the_signal_coalesced() {
        run(async || {
            let h = Harness::new(None);
            for s in ["one", "two", "three"] {
                h.source.set_text(s);
                h.poll();
                assert_eq!(h.drain().await, [text(s)]);
            }
        });
    }

    #[test]
    fn changed_event_supersedes_in_flight_poll() {
        run(async || {
            let h = Harness::new(None);
            let (open, gate) = async_channel::unbounded();
            h.source.0.borrow_mut().gate = Some(gate);
            h.source.set_text("old");
            h.poll(); // blocks on the gate
            h.source.set_text("new");
            h.source.emit_changed(); // cancels the poll, reads "new"
            open.send_blocking(()).unwrap();
            open.send_blocking(()).unwrap();
            assert_eq!(h.drain().await, [text("new")]);
        });
    }

    #[test]
    fn poll_skips_while_a_read_is_in_flight_until_timeout() {
        run(async || {
            let h = Harness::new(None);
            h.monitor.inner.poll_timeout.set(Duration::from_millis(50));
            let (open, gate) = async_channel::unbounded();
            h.source.0.borrow_mut().gate = Some(gate);
            h.source.set_text("stuck");
            h.poll();
            glib::timeout_future(Duration::from_millis(10)).await;
            h.poll(); // still in flight: skipped
            glib::timeout_future(Duration::from_millis(10)).await;
            assert_eq!(h.source.reads(), 1);
            glib::timeout_future(Duration::from_millis(50)).await;
            h.poll(); // the first read is now stuck; it's cancelled and retried
            glib::timeout_future(Duration::from_millis(10)).await;
            assert_eq!(h.source.reads(), 2);
            open.send_blocking(()).unwrap();
            open.send_blocking(()).unwrap();
            assert_eq!(h.drain().await, [text("stuck")]);
        });
    }

    #[test]
    fn changed_signal_captures_image_as_png() {
        run(async || {
            let h = Harness::new(None);
            let png = sample_png([255, 0, 0, 255]);
            h.source.set_image(&png);
            h.source.emit_changed();
            let got = h.drain().await;
            let [Capture::Image(captured)] = got.as_slice() else {
                panic!("expected one image, got {got:?}");
            };
            assert!(captured.starts_with(b"\x89PNG\r\n\x1a\n"));
            // Repeats are dropped by content hash.
            h.source.emit_changed();
            assert_eq!(h.drain().await, []);
        });
    }

    #[test]
    fn changed_signal_falls_back_to_raw_stream() {
        run(async || {
            let h = Harness::new(None);
            h.source.set_image(&sample_png([0, 255, 0, 255]));
            h.source.0.borrow_mut().texture = None;
            h.source.emit_changed();
            assert!(matches!(h.drain().await.as_slice(), [Capture::Image(_)]));
        });
    }

    #[test]
    fn poll_does_not_reencode_an_unchanged_image() {
        run(async || {
            let h = Harness::new(None);
            h.source.set_image(&sample_png([0, 0, 255, 255]));
            h.poll();
            assert!(matches!(h.drain().await.as_slice(), [Capture::Image(_)]));
            // Even with the delivery hash forgotten, the raw-bytes hash stops
            // the same image from being decoded and delivered again.
            h.monitor.inner.state.borrow_mut().last_hash = None;
            h.poll();
            assert_eq!(h.drain().await, []);
        });
    }

    #[test]
    fn poll_falls_back_to_text_when_image_read_fails() {
        run(async || {
            let h = Harness::new(None);
            {
                let mut f = h.source.0.borrow_mut();
                f.mimes = vec!["image/png".into(), "text/plain".into()];
                f.text = Some("actually text".into());
            }
            h.poll();
            assert_eq!(h.drain().await, [text("actually text")]);
        });
    }

    #[test]
    fn own_clipboard_writes_are_ignored() {
        run(async || {
            let h = Harness::new(None);
            h.source.set_text("ours");
            h.source.0.borrow_mut().local = true;
            h.source.emit_changed();
            h.poll();
            assert_eq!(h.drain().await, []);
            assert_eq!(h.source.reads(), 0);
        });
    }
}
