use transaction::DeliveryResult;

/// The trait that handles the completion of the transaction ([`DeliveryResult`]).
/// `complete()` is guaranteed to be called once the Transaction is spawned, even
/// if the thread panics or the future is dropped.
pub trait Deliverable: Send + 'static {
    fn complete(self, result: DeliveryResult);
}
