//! After a `101` the upstream read timeout of an HTTP/1 tunnel stays the peer's `read_timeout`,
//! and the application cannot end the tunnel.
//!
//! The peer's `read_timeout` is 30s, as a proxy would set it for slow response heads. The tests
//! expect an idle tunnel to be closed after 500ms and a tunnel the application cancels to end at
//! once. `ProxyHttp::tunnel` and `Tunnel` exist only with the patch, so with stock Pingora
//! the hook is left out: the idle tunnel is still open after 5s and the first test fails. The
//! second test needs the hook and is built only with `--features patched`.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pingora_core::Result;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_proxy::{ProxyHttp, Session};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::time::timeout;

const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
const TUNNEL_IDLE: Duration = Duration::from_millis(500);
const DEADLINE: Duration = Duration::from_secs(5);

struct Passthrough {
    upstream: SocketAddr,
    #[cfg_attr(not(feature = "patched"), allow(dead_code))]
    idle: Option<Duration>,
    #[cfg_attr(not(feature = "patched"), allow(dead_code))]
    cancel: Arc<Notify>,
}

#[async_trait]
impl ProxyHttp for Passthrough {
    type CTX = ();

    fn new_ctx(&self) {}

    async fn upstream_peer(&self, _: &mut Session, _: &mut ()) -> Result<Box<HttpPeer>> {
        let mut peer = HttpPeer::new(self.upstream, false, String::new());
        peer.options.read_timeout = Some(HEAD_TIMEOUT);
        Ok(Box::new(peer))
    }

    #[cfg(feature = "patched")]
    fn tunnel(&self, _: &Session, _: &mut ()) -> Option<pingora_proxy::Tunnel> {
        Some(pingora_proxy::Tunnel {
            read_timeout: self.idle,
            cancel: Arc::clone(&self.cancel),
        })
    }
}

/// Answers an upgrade with `101` and then stays silent until the client side closes.
async fn silent_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let _ = common::read_head(&mut stream).await;
                let reply = "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n";
                let _ = stream.write_all(reply.as_bytes()).await;
                let mut buf = [0_u8; 1024];
                while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
            });
        }
    });
    addr
}

async fn open_tunnel(proxy: SocketAddr) -> TcpStream {
    let mut client = common::connect(proxy).await;
    let request = "GET /chat HTTP/1.1\r\nHost: upstream.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n";
    client.write_all(request.as_bytes()).await.expect("write");
    let (head, _) = common::read_head(&mut client).await;
    assert!(
        head.starts_with("HTTP/1.1 101"),
        "expected 101, got {head:?}"
    );
    client
}

/// Waits for the proxy to close the tunnel; true if it did within `within`.
async fn closed_within(client: &mut TcpStream, within: Duration) -> bool {
    let mut buf = [0_u8; 64];
    match timeout(within, client.read(&mut buf)).await {
        Ok(Ok(0) | Err(_)) => true,
        Ok(Ok(_)) | Err(_) => false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_tunnel_is_closed_after_its_own_timeout() {
    let upstream = silent_upstream().await;
    let proxy = common::start_proxy(Passthrough {
        upstream,
        idle: Some(TUNNEL_IDLE),
        cancel: Arc::new(Notify::new()),
    });
    let mut client = open_tunnel(proxy).await;
    assert!(
        closed_within(&mut client, DEADLINE).await,
        "the idle tunnel was still open after {DEADLINE:?}; its read timeout stayed at {HEAD_TIMEOUT:?}"
    );
}

#[cfg(feature = "patched")]
#[tokio::test(flavor = "multi_thread")]
async fn cancel_ends_the_tunnel() {
    let upstream = silent_upstream().await;
    let cancel = Arc::new(Notify::new());
    let proxy = common::start_proxy(Passthrough {
        upstream,
        idle: None,
        cancel: Arc::clone(&cancel),
    });
    let mut client = open_tunnel(proxy).await;
    assert!(
        !closed_within(&mut client, Duration::from_millis(300)).await,
        "the tunnel closed before it was cancelled"
    );
    cancel.notify_one();
    assert!(
        closed_within(&mut client, Duration::from_secs(2)).await,
        "the tunnel was still open 2s after cancel"
    );
}
