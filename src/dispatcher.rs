use std::sync::mpsc;

use transaction::DeliveryResult;

/// The trait that handles the completion of the transaction (delivery result)
pub trait Dispatcher : Send + 'static {
    fn notify(self, result: DeliveryResult);
}

impl Dispatcher for mpsc::Sender<DeliveryResult> {
    fn notify(self, result: DeliveryResult) {
        let _ = self.send(result);
    }
}
