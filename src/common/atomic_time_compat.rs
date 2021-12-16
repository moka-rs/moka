use parking_lot::RwLock;
use super::time::Instant;

pub(crate) struct AtomicInstant {
    instant: RwLock<Option<Instant>>,
}

impl Default for AtomicInstant {
    fn default() -> Self {
        Self {
            instant: RwLock::new(None),
        }
    }
}

impl AtomicInstant {
    pub(crate) fn reset(&self) {
        *self.instant.write() = None;
    }

    pub(crate) fn is_set(&self) -> bool {
        self.instant.read().is_some()
    }

    pub(crate) fn instant(&self) -> Option<Instant> {
        *self.instant.read()
    }

    pub(crate) fn set_instant(&self, instant: Instant) {
        *self.instant.write() = Some(instant);
    }
}
