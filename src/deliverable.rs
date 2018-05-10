use std::sync::mpsc;

use transaction::DeliveryResult;

/// The trait that handles the completion of the transaction (delivery result)
pub trait Deliverable : Send + 'static {
    fn complete(self, result: DeliveryResult);
}

impl Deliverable for mpsc::Sender<DeliveryResult> {
    fn complete(self, result: DeliveryResult) {
        let _ = self.send(result);
    }
}
