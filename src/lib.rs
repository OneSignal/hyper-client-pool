#[macro_use] extern crate log;

extern crate fpool;
extern crate futures;
extern crate hyper_tls;
extern crate native_tls;
extern crate tokio_core;

pub extern crate hyper;

mod config;
mod counter;
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
    use std::thread;
    use std::time::Duration;

    use hyper::{Request, Method};
    use super::*;

    impl Deliverable for mpsc::Sender<DeliveryResult> {
        fn complete(self, result: DeliveryResult) {
            let _ = self.send(result);
        }
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

    impl Deliverable for SuccessfulCompletionCounter {
        fn complete(self, result: DeliveryResult) {
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
            Err(RequestError::Full(_transaction)) => (), // expected
            res => panic!("Expected Error::Full, got {:?}", res),
        }

        rx.recv().unwrap();
    }

    const ONE_SIGNAL_IP_ADDRESSES : [&'static str; 5] = [
        "104.16.208.165",
        "104.16.204.165",
        "104.16.205.165",
        "104.16.207.165",
        "104.16.206.165",
    ];

    fn onesignal_connection_open() -> (bool, String) {
        let output = Command::new("lsof")
            .args(&["-i", "4tcp"])
            .output()
            .expect("command works");

        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines : Vec<_> = stdout.split("\n").filter(|line| line.starts_with("hyper")).collect();
        let stdout = lines.join("\n");

        (lines.len() > 0 && ONE_SIGNAL_IP_ADDRESSES.iter().any(|addr| lines[0].contains(addr)), stdout)
    }

    fn assert_onesignal_connection_open(expected_open: bool) {
        let (open, stdout) = onesignal_connection_open();
        assert_eq!(expected_open, open, "Output:\n{}", stdout);
    }

    #[test]
    fn keep_alive_works_as_expected() {
        // block until no connections are open - this is unfortunate..
        // but at least we have tests covering the keep-alive :)
        while onesignal_connection_open().0 {}

        let _ = env_logger::try_init();

        let mut config = Config::default();
        config.keep_alive_timeout = Duration::from_secs(2);

        let mut pool = Pool::new(config).unwrap();
        let (tx, rx) = mpsc::channel();

        // Start first request
        pool.request(
            Transaction::new(tx.clone(), Request::new(Method::Get, "https://onesignal.com/".parse().unwrap()))
        ).expect("request ok");

        // wait for request to finish
        rx.recv().unwrap();
        assert_onesignal_connection_open(true);
        thread::sleep(Duration::from_secs(2));
        assert_onesignal_connection_open(true);
        thread::sleep(Duration::from_secs(2));
        assert_onesignal_connection_open(false);
    }
}
