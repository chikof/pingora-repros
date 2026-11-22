//! Shared by the repros: an in-process Pingora proxy and small raw-socket helpers.

#![allow(dead_code)]

use std::net::{SocketAddr, TcpListener as StdListener};
use std::thread;
use std::time::{Duration, Instant};

use pingora_core::server::Server;
use pingora_core::server::configuration::Opt;
use pingora_proxy::{ProxyHttp, http_proxy_service};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpStream;

/// Starts a proxy running `app` on a free 127.0.0.1 port and returns the port's address once it
/// accepts connections. The server thread lives until the test process exits.
pub fn start_proxy<P>(app: P) -> SocketAddr
where
    P: ProxyHttp + Send + Sync + 'static,
    P::CTX: Send + Sync,
{
    // Pingora cannot take an already bound listener here, so reserve a port and release it.
    let addr = StdListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("reserve a port");
    thread::spawn(move || {
        let mut server = Server::new(Some(Opt::default())).expect("server");
        server.bootstrap();
        let mut service = http_proxy_service(&server.configuration, app);
        service.add_tcp(&addr.to_string());
        server.add_service(service);
        server.run_forever()
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(addr).is_err() {
        assert!(Instant::now() < deadline, "the proxy did not start");
        thread::sleep(Duration::from_millis(20));
    }
    addr
}

/// Reads from `stream` until the end of an HTTP head (`\r\n\r\n`) and returns the head with
/// anything read past it.
pub async fn read_head<R: AsyncRead + Unpin>(stream: &mut R) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return (String::from_utf8(buf).expect("utf-8 head"), rest);
        }
        let n = stream.read(&mut chunk).await.expect("read head");
        assert!(n > 0, "connection closed inside the head: {buf:?}");
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Opens a connection to `addr`.
pub async fn connect(addr: SocketAddr) -> TcpStream {
    TcpStream::connect(addr).await.expect("connect")
}
