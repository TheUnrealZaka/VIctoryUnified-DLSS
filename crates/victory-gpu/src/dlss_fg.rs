//! Cross-platform configuration and lifecycle bridge for DLSS Frame Generation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use victory_core::config::{DlssFgConfig, DlssFgMode};

static CONFIG: RwLock<Option<Arc<DlssFgConfig>>> = RwLock::new(None);
/// Whether the hooks have anything to do (frame generation on, or the probe): asked on every
/// draw, so it is a flag and not a read of the configuration.
static WANTED: AtomicBool = AtomicBool::new(false);

/// Sets the DLSS-FG configuration before host GPU creation.
pub fn configure(cfg: DlssFgConfig) {
    WANTED.store(cfg.mode != DlssFgMode::Off || cfg.hooks.probe, Ordering::Release);
    *CONFIG.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(cfg));
}

/// Returns the current DLSS-FG configuration (default: disabled). Shared, not copied: draws ask.
pub fn config() -> Arc<DlssFgConfig> {
    CONFIG
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// Whether frame generation or the hook probe is asked for.
pub(crate) fn wanted() -> bool {
    WANTED.load(Ordering::Acquire)
}

/// Shuts down Streamline cleanly when the title process exits normally.
pub fn shutdown() {
    #[cfg(windows)]
    crate::streamline::shutdown();
}
