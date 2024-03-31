use std::sync::atomic::{AtomicU32, Ordering};

/// Counter that returns unique IDs.
///
/// Under the hood it uses a 24-bit ID that will eventually wrap around. When incrementing it once a
/// second, it will wrap around after about 194 days.
pub struct IdCounter {
    value: AtomicU32,
}

impl IdCounter {
    pub const fn new() -> Self {
        Self {
            value: AtomicU32::new(1),
        }
    }

    pub fn next(&self) -> u32 {
        // Wrap around at 24 bits; make sure to always start at one.
        let mut v = self.value.fetch_add(1, Ordering::SeqCst);
        while v >= (1 << 24) {
            v = match self
                .value
                .compare_exchange(v, 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => self.value.fetch_add(1, Ordering::SeqCst),
                Err(x) => x,
            };
        }
        v
    }
}

impl Default for IdCounter {
    fn default() -> Self {
        Self::new()
    }
}
