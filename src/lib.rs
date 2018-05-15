#[cfg(test)] #[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;

extern crate fpool;
extern crate futures;
extern crate hyper_tls;
extern crate native_tls;
extern crate raii_counter;
extern crate tokio_core;

pub extern crate hyper;

mod config;
mod deliverable;
mod error;
mod executor;
mod pool;
mod transaction;

pub use deliverable::Deliverable;
pub use transaction::{Transaction, DeliveryResult};
pub use pool::Pool;
pub use error::{RequestError, SpawnError};
pub use config::Config;

#[cfg(test)]
mod tests {
    extern crate env_logger;

    use std::process::Command;
    use std::sync::{Arc, mpsc};
    use std::sync::atomic::{Ordering, AtomicUsize};
    use std::sync::RwLock;
    use std::thread;
    use std::time::Duration;

    use hyper::{Request, Method};
    use super::*;

    lazy_static! {
        /// For tests that depend on global state (ahem - keep_alive_works_as_expected())
        /// we have this test_lock which they can grab with a `write()` in order to ensure
        /// no other tests in this file are running
        static ref TEST_LOCK: RwLock<()> = RwLock::new(());
    }

    impl Deliverable for mpsc::Sender<DeliveryResult> {
        fn complete(self, result: DeliveryResult) {
            let _ = self.send(result);
        }
    }

    fn onesignal_transaction<D: Deliverable>(deliverable: D) -> Transaction<D> {
        Transaction::new(deliverable, Request::new(Method::Get, "https://onesignal.com/".parse().unwrap()))
    }

    fn assert_successful_result(result: DeliveryResult) {
        match result {
            DeliveryResult::Response { response, .. } => {
                assert!(response.status().is_success(), format!("Expected successful response: {:?}", response.status()));
            },
            res => panic!("Expected DeliveryResult::Response, unexpected delivery result: {:?}", res),
        }
    }

    #[test]
    fn lots_of_get_single_worker() {
        let _read = TEST_LOCK.read().unwrap();

        let _ = env_logger::try_init();

        let mut config = Config::default();
        config.workers = 1;

        let mut pool = Pool::new(config).unwrap();
        let (tx, rx) = mpsc::channel();

        for _ in 0..5 {
            pool.request(onesignal_transaction(tx.clone())).expect("request ok");
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

    impl Deliverable for SuccessfulCompletionCounter {
        fn complete(self, result: DeliveryResult) {
            assert_successful_result(result);
            self.count.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn graceful_shutdown() {
        let _read = TEST_LOCK.read().unwrap();

        let _ = env_logger::try_init();

        let txn = 20;
        let counter = SuccessfulCompletionCounter::new();

        let mut config = Config::default();
        config.workers = 2;

        let mut pool = Pool::new(config).unwrap();
        for _ in 0..txn {
            pool.request(onesignal_transaction(counter.clone())).expect("request ok");
        }

        pool.shutdown();
        assert_eq!(counter.count(), txn);
    }

    #[test]
    fn full_error() {
        let _read = TEST_LOCK.read().unwrap();

        let _ = env_logger::try_init();

        let mut config = Config::default();
        config.workers = 1;
        config.max_transactions_per_worker = 1;

        let mut pool = Pool::new(config).unwrap();
        let (tx, rx) = mpsc::channel();

        // Start first request
        pool.request(onesignal_transaction(tx.clone())).expect("request ok");

        match pool.request(onesignal_transaction(tx.clone())) {
            Err(RequestError::Full(_transaction)) => (), // expected
            res => panic!("Expected Error::Full, got {:?}", res),
        }

        rx.recv().unwrap();
    }

    const ONE_SIGNAL_IP_ADDRESSES : [&'static str; 10] = [
        // IPv4 addresses
        "104.16.208.165",
        "104.16.204.165",
        "104.16.205.165",
        "104.16.207.165",
        "104.16.206.165",

        // IPv6 addresses
        "2400:cb00:2048:1::6810:cda5",
        "2400:cb00:2048:1::6810:cca5",
        "2400:cb00:2048:1::6810:cea5",
        "2400:cb00:2048:1::6810:d0a5",
        "2400:cb00:2048:1::6810:cfa5",
    ];

    fn onesignal_connection_count() -> (usize, String) {
        let output = Command::new("lsof")
            .args(&["-i"])
            .output()
            .expect("command works");

        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines : Vec<_> = stdout.split("\n").filter(|line| {
            line.starts_with("hyper") &&
                ONE_SIGNAL_IP_ADDRESSES.iter().any(|addr| line.contains(addr))
        }).collect();
        let stdout = lines.join("\n");

        (lines.len(), stdout)
    }

    macro_rules! assert_onesignal_connection_open_count_eq {
        ($expected_open_count:expr) => {
            let (open_count, stdout) = onesignal_connection_count();
            assert_eq!($expected_open_count, open_count, "Output:\n{}", stdout);
        };
    }

    #[test]
    fn keep_alive_works_as_expected() {
        let _write = TEST_LOCK.write().unwrap();

        // block until no connections are open - this is unfortunate..
        // but at least we have tests covering the keep-alive :)
        while onesignal_connection_count().0 > 0 {}

        let _ = env_logger::try_init();

        let mut config = Config::default();
        config.keep_alive_timeout = Duration::from_secs(3);

        let mut pool = Pool::new(config).unwrap();
        let (tx, rx) = mpsc::channel();

        // Start first request
        pool.request(onesignal_transaction(tx.clone())).expect("request ok");

        // wait for request to finish
        rx.recv().unwrap();
        assert_onesignal_connection_open_count_eq!(1);
        thread::sleep(Duration::from_secs(1));
        assert_onesignal_connection_open_count_eq!(1);

        thread::sleep(Duration::from_secs(5));
        // keep-alive should kill connection by now
        assert_onesignal_connection_open_count_eq!(0);
    }

    #[test]
    fn timeout_works_as_expected() {
        let _read = TEST_LOCK.read().unwrap();

        let _ = env_logger::try_init();

        let mut config = Config::default();
        config.transaction_timeout = Duration::from_secs(2);

        let mut pool = Pool::new(config).unwrap();
        let (tx, rx) = mpsc::channel();

        // Start first request
        pool.request(
            // This endpoint will not return for a while, therefore should timeout
            Transaction::new(tx.clone(), Request::new(Method::Get, "https://httpstat.us/200?sleep=5000".parse().unwrap()))
        ).expect("request ok");

        match rx.recv().unwrap() {
            DeliveryResult::Timeout { .. } => (), // ok
            res => panic!("Expected timeout!, got: {:?}", res),
        }
    }
}
