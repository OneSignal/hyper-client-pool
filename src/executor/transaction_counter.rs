use raii_counter::WeakCounter;

pub struct TransactionCounter {
    transaction_counter: WeakCounter,
    worker_counter: WeakCounter,
}

impl TransactionCounter {
    pub(crate) fn new(
        transaction_counter: WeakCounter,
        worker_counter: WeakCounter
    ) -> TransactionCounter {
        TransactionCounter { transaction_counter, worker_counter }
    }

    /// Get the count of transactions
    pub fn count(&self) -> usize {
        self.transaction_counter.count()
    }

    /// Checks that the Worker is still alive
    /// The strong counter is held by the Worker
    pub fn is_valid(&self) -> bool {
        self.worker_counter.count() > 0
    }
}
