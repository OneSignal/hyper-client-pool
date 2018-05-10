use transaction::DeliveryResult;

/// The trait that handles the completion of the transaction (delivery result)
pub trait Deliverable : Send + 'static {
    fn complete(self, result: DeliveryResult);
}
