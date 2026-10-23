//! Clipbrain: a clipboard history manager for GNOME.

// Phase 1 only exercises storage from tests; the GUI wiring comes later.
#[allow(dead_code)]
mod storage;

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("CLIPBRAIN_LOG", "warn")).init();

    let store = storage::Store::open_default().context("opening clipboard history")?;
    log::info!("loaded {} history entries", store.entries().len());
    Ok(())
}
