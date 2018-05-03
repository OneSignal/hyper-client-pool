use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// This is essentially an AtomicUsize that is clonable and whose count is based
/// on the number of copies. The count is automaticaly updated on drop.
pub struct Counter(Arc<AtomicUsize>);

impl Counter {
    /// Get the count
    ///
    /// This method is inherently racey. Assume the count will have changed once
    /// the value is observed.
    #[inline]
    pub fn get(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    /// Create a new RAII counter
    pub fn new() -> Counter {
        // Note the value in the Arc doesn't matter since we rely on the Arc's
        // strong count to provide a value.
        Counter(Arc::new(AtomicUsize::new(1)))
    }
}

impl Clone for Counter {
    fn clone(&self) -> Counter {
        let count = self.0.clone();
        count.fetch_add(1, Ordering::AcqRel);
        Counter(count)
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
