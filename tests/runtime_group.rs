//! `Server::run_forever` builds one tokio runtime per service, each with `ServerConf.threads`
//! workers. A proxy with one application per listener therefore runs one worker pool per
//! listener.
//!
//! The test runs a server with three listening services and one background service, all with
//! `threads = 3`, then counts the process's threads whose name starts with `rg` in
//! `/proc/self/task/*/comm`. Pingora names a runtime's threads after the runtime, which is the
//! service on stock Pingora. Stock Pingora runs 4 services x 3 workers = 12 threads, so the test
//! fails. With `--features patched` every service, the background one included, sets
//! `runtime_group` to `rg`, they share one runtime and the count is 3. Tokio's blocking pool
//! threads carry the same name, so the patched count may exceed 3, but never reaches 6.
//!
//! Linux only: it reads `/proc`.

#![cfg(target_os = "linux")]

use std::fs;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pingora_core::Result;
use pingora_core::server::{Server, ShutdownWatch};
use pingora_core::server::configuration::ServerConf;
use pingora_core::services::background::{BackgroundService, GenBackgroundService};
use pingora_core::upstreams::peer::HttpPeer;
use pingora_proxy::{ProxyHttp, Session, http_proxy_service_with_name};

const SERVICES: [&str; 3] = ["rg-a", "rg-b", "rg-c"];
const THREADS: usize = 3;
/// A service that is not a listener, such as an HTTP/3 front or a UDP loop.
const BACKGROUND: &str = "rg-bg";

struct Unused;

#[async_trait]
impl ProxyHttp for Unused {
    type CTX = ();

    fn new_ctx(&self) {}

    async fn upstream_peer(&self, _: &mut Session, _: &mut ()) -> Result<Box<HttpPeer>> {
        Ok(Box::new(HttpPeer::new("127.0.0.1:9", false, String::new())))
    }
}

/// Set by the background service once it runs, so the test counts threads only after its
/// runtime exists.
static STARTED: AtomicBool = AtomicBool::new(false);

struct Idle;

#[async_trait]
impl BackgroundService for Idle {
    async fn start(&self, _shutdown: ShutdownWatch) {
        STARTED.store(true, Ordering::SeqCst);
    }
}

fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("reserve a port")
}

/// Starts one listening service per name in `SERVICES`, each on its own port, plus the
/// background service `BACKGROUND`, and returns once every port accepts connections and the
/// background service runs.
fn start_server() {
    let addrs: Vec<SocketAddr> = SERVICES.iter().map(|_| free_addr()).collect();
    let listen = addrs.clone();
    thread::spawn(move || {
        let conf = ServerConf {
            threads: THREADS,
            ..ServerConf::default()
        };
        let mut server = Server::new_with_opt_and_conf(None, conf);
        server.bootstrap();
        for (name, addr) in SERVICES.iter().zip(&listen) {
            let mut service = http_proxy_service_with_name(&server.configuration, Unused, name);
            service.add_tcp(&addr.to_string());
            #[cfg(feature = "patched")]
            {
                service.runtime_group = Some("rg".to_string());
            }
            server.add_service(service);
        }
        let mut background = GenBackgroundService::new(BACKGROUND.to_string(), Arc::new(Idle));
        background.threads = Some(THREADS);
        #[cfg(feature = "patched")]
        {
            background.runtime_group = Some("rg".to_string());
        }
        server.add_service(background);
        server.run_forever()
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    for addr in addrs {
        while TcpStream::connect(addr).is_err() {
            assert!(Instant::now() < deadline, "the server did not start");
            thread::sleep(Duration::from_millis(20));
        }
    }
    while !STARTED.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the background service did not start");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The names of this process's threads that start with `rg`.
fn worker_names() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir("/proc/self/task")
        .expect("list threads")
        .filter_map(|task| fs::read_to_string(task.ok()?.path().join("comm")).ok())
        .map(|comm| comm.trim_end().to_string())
        .filter(|comm| comm.starts_with("rg"))
        .collect();
    names.sort();
    names
}

#[test]
fn services_share_one_worker_pool() {
    start_server();
    let names = worker_names();
    let count = names.len();
    assert!(
        (THREADS..2 * THREADS).contains(&count),
        "{} services with threads = {THREADS} started {count} worker threads ({names:?}); \
         each service got its own pool, and only {THREADS} were wanted",
        SERVICES.len() + 1,
    );
}
