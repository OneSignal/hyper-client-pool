#[macro_use] extern crate log;

extern crate fpool;
extern crate futures;
extern crate hyper_tls;
extern crate hyper;
extern crate native_tls;
extern crate tokio_core;

mod config;
mod counter;
mod dispatcher;
mod error;
mod executor;
mod pool;
mod transaction;

pub use dispatcher::Dispatcher;
pub use transaction::{Transaction, DeliveryResult};
pub use pool::Pool;
pub use error::Error;
pub use config::Config;

#[cfg(test)]
mod tests {
    extern crate env_logger;

    use std::sync::{Arc, mpsc};
    use std::sync::atomic::{Ordering, AtomicUsize};

    use hyper::{Request, Method};
    use super::*;

    fn assert_successful_result(result: DeliveryResult) {
        match result {
            DeliveryResult::Response { inner, duration: _ } => {
                assert!(inner.status().is_success(), format!("Expected successful response: {:?}", inner));
            },
            res => panic!("Expected DeliveryResult::Response, unexpected delivery result: {:?}", res),
        }
    }

    #[test]
    fn lots_of_get_single_worker() {
        let _ = env_logger::try_init();

        let mut config = Config::default();
        config.workers = 2;

        let mut pool = Pool::new(config).unwrap();
        let (tx, rx) = mpsc::channel();

        for _ in 0..5 {
            pool.request(
                Transaction::new(
                    tx.clone(),
                    Request::new(Method::Get, "https://onesignal.com/".parse().unwrap()),
                )
            ).expect("request ok");
        }

        for _ in 0..5 {
            assert_successful_result(rx.recv().unwrap());
        }
    }

    #[derive(Debug, Clone)]
    struct SuccessfulCompletionCounter {
        count: Arc<AtomicUsize>,
    }

    impl SuccessfulCompletionCounter {
        fn new() -> SuccessfulCompletionCounter {
            SuccessfulCompletionCounter { count: Arc::new(AtomicUsize::new(0)) }
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::Acquire)
        }
    }

    impl Dispatcher for SuccessfulCompletionCounter {
        fn notify(&mut self, result: DeliveryResult) {
            assert_successful_result(result);
            self.count.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn graceful_shutdown() {
        let _ = env_logger::try_init();

        let txn = 20;
        let counter = SuccessfulCompletionCounter::new();

        let mut config = Config::default();
        config.workers = 2;

        let mut pool = Pool::new(config).unwrap();
        for _ in 0..txn {
            pool.request(
                Transaction::new(
                    counter.clone(),
                    Request::new(Method::Get, "https://onesignal.com/".parse().unwrap()),
                )
            ).expect("request ok");
        }

        pool.shutdown();
        assert_eq!(counter.count(), txn);
    }

    #[test]
    fn full_error() {
        let _ = env_logger::try_init();

        let mut config = Config::default();
        config.workers = 1;
        config.max_transactions_per_worker = 1;

        let mut pool = Pool::new(config).unwrap();
        let (tx, rx) = mpsc::channel();

        // Start first request
        pool.request(
            Transaction::new(
                tx.clone(),
                Request::new(Method::Get, "https://onesignal.com/".parse().unwrap()),
            )
        ).expect("request ok");

        match pool.request(
            Transaction::new(
                tx.clone(),
                Request::new(Method::Get, "https://onesignal.com/".parse().unwrap()),
            )
        )
        {
            Err(Error::Full(_transaction)) => (), // expected
            res => panic!("Expected Error::Full, got {:?}", res),
        }

        rx.recv().unwrap();
    }
}
