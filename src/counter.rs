use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// This is essentially an AtomicUsize that is clonable and whose count is based
/// on the number of copies. The count is automaticaly updated on drop.
pub struct Counter(Arc<AtomicUsize>);

#[derive(Clone)]
pub struct WeakCounter(Arc<AtomicUsize>);

impl Counter {
    /// Create a new RAII counter
    pub fn new() -> Counter {
        Counter(Arc::new(AtomicUsize::new(1)))
    }

    pub fn downgrade(self) -> WeakCounter {
        WeakCounter(self.0.clone())
    }
}

impl WeakCounter {
    /// Get the count
    ///
    /// This method is inherently racey. Assume the count will have changed once
    /// the value is observed.
    #[inline]
    pub fn get(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    pub fn upgrade(self) -> Counter {
        self.0.fetch_add(1, Ordering::AcqRel);
        Counter(self.0.clone())
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
