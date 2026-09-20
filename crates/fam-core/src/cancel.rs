use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cooperative stop signal: enumeration polls it between directories and between emitted entries,
/// which is what the legacy scan never had — its timeout only made the caller give up while the
/// walk kept running (`docs/legacy-specs.md` §2.2-4).
#[derive(Debug, Clone, Default)]
pub struct Cancellation {
    stop: Arc<AtomicBool>,
}

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_the_signal() {
        let cancel = Cancellation::new();
        let clone = cancel.clone();
        assert!(!clone.is_stopped());
        cancel.cancel();
        assert!(clone.is_stopped());
    }
}
