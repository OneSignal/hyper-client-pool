#[macro_use]
extern crate lazy_static;

extern crate env_logger;
extern crate hyper_client_pool;
extern crate ipnet;
extern crate regex;

use std::net::IpAddr;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use hyper::{Body, Request};
use hyper_client_pool::*;
use ipnet::{Contains, IpNet};
use regex::Regex;

lazy_static! {
    /// For tests that depend on global state (ahem - keep_alive_works_as_expected())
    /// we have this test_lock which they can grab with a `write()` in order to ensure
    /// no other tests in this file are running
    static ref TEST_LOCK: RwLock<()> = RwLock::new(());
}

#[derive(Debug)]
struct MspcDeliverable(mpsc::Sender<DeliveryResult>);

impl Deliverable for MspcDeliverable {
    fn complete(self, result: DeliveryResult) {
        let _ = self.0.send(result);
    }
}

fn default_config() -> Config {
    Config {
        keep_alive_timeout: Duration::from_secs(3),
        transaction_timeout: Duration::from_secs(20),
        dns_threads_per_worker: 1,
        max_transactions_per_worker: 1_000,
        workers: 2,
    }
}

fn onesignal_transaction<D: Deliverable>(deliverable: D) -> Transaction<D> {
    Transaction::new(
        deliverable,
        Request::get("https://onesignal.com/")
            .body(Body::empty())
            .unwrap(),
        false,
    )
}

fn httpbin_transaction<D: Deliverable>(deliverable: D) -> Transaction<D> {
    Transaction::new(
        deliverable,
        Request::get("http://httpbin:80/ip")
            .body(Body::empty())
            .unwrap(),
        false,
    )
}

fn check_successful_result(result: DeliveryResult) -> (bool, DeliveryResult) {
    let successful = match result {
        DeliveryResult::Response { ref response, .. } => response.status().is_success(),
        _ => false,
    };
    (successful, result)
}

fn assert_successful_result(result: DeliveryResult) {
    let (successful, result) = check_successful_result(result);
    assert_eq!(true, successful, "Not successful result: {:?}!", result);
}

#[test]
fn some_gets_single_worker() {
    let _read = TEST_LOCK.read().unwrap_or_else(|e| e.into_inner());

    let _ = env_logger::try_init();

    let mut config = default_config();
    config.workers = 1;

    let mut pool = Pool::builder(config).build().unwrap();
    let (tx, rx) = mpsc::channel();

    for _ in 0..5 {
        pool.request(onesignal_transaction(MspcDeliverable(tx.clone())))
            .expect("request ok");
    }

    for _ in 0..5 {
        assert_successful_result(rx.recv().unwrap());
    }

    pool.shutdown();
}

#[test]
fn ton_of_gets() {
    const REQUEST_AMOUNT: usize = 600;
    let _read = TEST_LOCK.read().unwrap_or_else(|e| e.into_inner());

    let _ = env_logger::try_init();

    let mut config = default_config();
    config.dns_threads_per_worker = 4;
    config.workers = 4;
    config.max_transactions_per_worker = 1_000;
    config.transaction_timeout = Duration::from_secs(60);

    let mut pool = Pool::builder(config).build().unwrap();
    let (tx, rx) = mpsc::channel();

    for _ in 0..REQUEST_AMOUNT {
        pool.request(httpbin_transaction(MspcDeliverable(tx.clone())))
            .expect("request ok");
    }

    let mut successes = 0;
    let mut not_successes = 0;
    for _ in 0..REQUEST_AMOUNT {
        let (successful, _result) = check_successful_result(rx.recv().unwrap());
        if successful {
            successes += 1;
        } else {
            not_successes += 1;
        }
    }

    assert!(successes > ((REQUEST_AMOUNT as f32) * 0.9) as _);
    assert!(not_successes < ((REQUEST_AMOUNT as f32) * 0.1) as _);

    pool.shutdown();
    println!(
        "Successes: {} | Not Successes: {}",
        successes, not_successes
    );
}

#[derive(Debug, Clone)]
struct SuccessfulCompletionCounter {
    count: Arc<AtomicUsize>,
}

impl SuccessfulCompletionCounter {
    fn new() -> SuccessfulCompletionCounter {
        SuccessfulCompletionCounter {
            count: Arc::new(AtomicUsize::new(0)),
        }
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
    let _read = TEST_LOCK.read().unwrap_or_else(|e| e.into_inner());

    let _ = env_logger::try_init();

    let txn = 20;
    let counter = SuccessfulCompletionCounter::new();

    let mut config = default_config();
    config.workers = 2;

    let mut pool = Pool::builder(config).build().unwrap();
    for _ in 0..txn {
        pool.request(onesignal_transaction(counter.clone()))
            .expect("request ok");
    }

    pool.shutdown();
    assert_eq!(counter.count(), txn);
}

#[test]
fn full_error() {
    let _read = TEST_LOCK.read().unwrap_or_else(|e| e.into_inner());

    let _ = env_logger::try_init();

    let mut config = default_config();
    config.workers = 3;
    config.max_transactions_per_worker = 1;

    let mut pool = Pool::builder(config).build().unwrap();
    let (tx, rx) = mpsc::channel();

    // Start requests
    for _ in 0..3 {
        pool.request(onesignal_transaction(MspcDeliverable(tx.clone())))
            .expect("request ok");
    }

    match pool.request(onesignal_transaction(MspcDeliverable(tx.clone()))) {
        Err(err) => assert_eq!(err.kind, ErrorKind::PoolFull),
        _ => panic!("Expected Error, got success request!"),
    }

    for _ in 0..3 {
        assert_successful_result(rx.recv().unwrap());
    }

    pool.shutdown();
}

static CLOUDFLARE_NETS: &[&str] = &[
    // IPv4
    "103.21.244.0/22",
    "103.22.200.0/22",
    "103.31.4.0/22",
    "104.16.0.0/12",
    "108.162.192.0/18",
    "131.0.72.0/22",
    "141.101.64.0/18",
    "162.158.0.0/15",
    "172.64.0.0/13",
    "173.245.48.0/20",
    "188.114.96.0/20",
    "190.93.240.0/20",
    "197.234.240.0/22",
    "198.41.128.0/17",
    // IPv6
    "2400:cb00::/32",
    "2405:8100::/32",
    "2405:b500::/32",
    "2606:4700::/32",
    "2803:f800::/32",
    "2c0f:f248::/32",
    "2a06:98c0::/29",
];

lazy_static! {
    static ref CLOUDFLARE_PARSED_NETS: Vec<IpNet> = {
        CLOUDFLARE_NETS
            .iter()
            .map(|net| net.parse::<IpNet>())
            .collect::<Result<Vec<IpNet>, _>>()
            .unwrap()
    };
    static ref LSOF_PARSE_IP_REGEX: Regex = { Regex::new(r"->\[?([^\]]*)\]?:https").unwrap() };
}

fn matches_cloudflare_ip(input: &str) -> bool {
    if let Some(captures) = LSOF_PARSE_IP_REGEX.captures(input) {
        match captures[1].parse::<IpAddr>() {
            Ok(addr) => CLOUDFLARE_PARSED_NETS.iter().any(|net| net.contains(&addr)),
            Err(_err) => false,
        }
    } else {
        false
    }
}

#[test]
fn matches_cloudflare_ip_works_as_expected() {
    // Test-case from staging
    let input1 = "hyper_cli 29606 deploy    9u  IPv6 74567336      0t0  TCP onepush-test-darren:46286->[2400:cb00:2048:1::6810:cea5]:https (ESTABLISHED)";
    assert_eq!(matches_cloudflare_ip(input1), true);
    // test-case from personal mac
    let input2 = "lib-f13ca 83600 darrentsung   12u  IPv4 0x2aad2644e2239ff9      0t0  TCP 192.168.2.240:54285->104.16.207.165:https (ESTABLISHED)";
    assert_eq!(matches_cloudflare_ip(input2), true);
}

fn onesignal_connection_count() -> (usize, String) {
    let output = Command::new("lsof")
        .args(&["-i"])
        .output()
        .expect("command works");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let matching_lines = stdout
        .split("\n")
        .filter(|line| matches_cloudflare_ip(line))
        .count();

    (matching_lines, stdout)
}

macro_rules! assert_onesignal_connection_open_count_eq {
    ($expected_open_count:expr) => {
        let (open_count, stdout) = onesignal_connection_count();
        assert_eq!($expected_open_count, open_count, "Output:\n{}", stdout);
    };
}

#[test]
fn keep_alive_works_as_expected() {
    let _write = TEST_LOCK.write().unwrap_or_else(|e| e.into_inner());

    // block until no connections are open - this is unfortunate..
    // but at least we have tests covering the keep-alive :)
    while onesignal_connection_count().0 > 0 {}

    let _ = env_logger::try_init();

    let mut config = default_config();
    config.keep_alive_timeout = Duration::from_secs(3);

    let mut pool = Pool::builder(config).build().unwrap();
    let (tx, rx) = mpsc::channel();

    // Start first request
    pool.request(onesignal_transaction(MspcDeliverable(tx.clone())))
        .expect("request ok");

    // wait for request to finish
    assert_successful_result(rx.recv().unwrap());
    let start = Instant::now();
    loop {
        let seconds_elapsed = start.elapsed().as_secs();
        if seconds_elapsed < 2 {
            assert_onesignal_connection_open_count_eq!(1);
        } else if seconds_elapsed > 3 {
            // keep-alive should kill connection by now
            assert_onesignal_connection_open_count_eq!(0);
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    pool.shutdown();
}

#[test]
fn connection_reuse_works_as_expected() {
    let _write = TEST_LOCK.write().unwrap_or_else(|e| e.into_inner());

    // block until no connections are open - this is unfortunate..
    // but at least we have tests covering the keep-alive :)
    while onesignal_connection_count().0 > 0 {}

    let _ = env_logger::try_init();

    let mut config = default_config();
    // note that workers must be one otherwise the second transaction will be
    // routed to another worker and a new connection will be established
    config.workers = 1;
    config.keep_alive_timeout = Duration::from_secs(10);

    let mut pool = Pool::builder(config).build().unwrap();
    let (tx, rx) = mpsc::channel();

    // Start first request
    pool.request(onesignal_transaction(MspcDeliverable(tx.clone())))
        .expect("request ok");
    // wait for request to finish
    assert_successful_result(rx.recv().unwrap());

    assert_onesignal_connection_open_count_eq!(1);
    thread::sleep(Duration::from_secs(3));
    assert_onesignal_connection_open_count_eq!(1);

    // Start second request
    pool.request(onesignal_transaction(MspcDeliverable(tx.clone())))
        .expect("request ok");
    // wait for request to finish
    assert_successful_result(rx.recv().unwrap());

    // there should only be one connection open
    assert_onesignal_connection_open_count_eq!(1);

    pool.shutdown();
}

#[test]
fn timeout_works_as_expected() {
    let _read = TEST_LOCK.read().unwrap_or_else(|e| e.into_inner());

    let _ = env_logger::try_init();

    let mut config = default_config();
    config.transaction_timeout = Duration::from_secs(2);

    let mut pool = Pool::builder(config).build().unwrap();
    let (tx, rx) = mpsc::channel();

    // Start first request
    pool.request(
        // This endpoint will not return for a while, therefore should timeout
        Transaction::new(
            MspcDeliverable(tx.clone()),
            Request::get("https://httpstat.us/200?sleep=5000")
                .body(Body::empty())
                .unwrap(),
            false,
        ),
    ).expect("request ok");

    match rx.recv().unwrap() {
        DeliveryResult::Timeout { .. } => (), // ok
        res => panic!("Expected timeout!, got: {:?}", res),
    }

    pool.shutdown();
}

#[test]
fn transaction_counting_works() {
    let _read = TEST_LOCK.read().unwrap_or_else(|e| e.into_inner());

    let _ = env_logger::try_init();

    let mut config = default_config();
    config.workers = 3;

    let transaction_counters = Arc::new(RwLock::new(Vec::new()));

    let mut pool = Pool::builder(config)
        .transaction_counters(transaction_counters.clone())
        .build()
        .unwrap();
    let (tx, rx) = mpsc::channel();

    // Start requests
    for _ in 0..3 {
        pool.request(onesignal_transaction(MspcDeliverable(tx.clone())))
            .expect("request ok");
    }

    let mut saw_transactions = false;
    let mut received = 0;
    loop {
        let counters = transaction_counters.try_read().unwrap();
        for counter in counters.iter() {
            assert_eq!(counter.is_valid(), true);
            let transaction_count = counter.count();
            if transaction_count == 1 {
                saw_transactions = true;
            }
            assert!(transaction_count <= 1);
        }

        if let Ok(recv) = rx.try_recv() {
            assert_successful_result(recv);
            received += 1;

            if received == 3 {
                break;
            }
        }
    }

    // Make sure that we saw the counter was doing something
    assert!(saw_transactions);

    pool.shutdown();

    // Counters should not be valid anymore
    let counters = transaction_counters.try_read().unwrap();
    for counter in counters.iter() {
        assert_eq!(counter.is_valid(), false);
    }
}
