//! An HTTP/1 upgrade whose `101` is read before the request's end-of-body task loses its tunnel.
//!
//! An upgrade `GET` has no body, so the proxy queues an end-of-body task for it. If the upstream's
//! `101` is read before that task is taken, the task then arrives on an upgraded connection and is
//! taken for the end of the tunnel. The `101` wins when anything before the queueing awaits
//! (here a `request_body_filter` that sleeps 10ms) or when the upstream answers within
//! microseconds. The sleep makes the order certain, so the test does not rely on timing luck.
//!
//! The client waits for the `101`, sends four bytes and expects them back. On stock
//! pingora-proxy 0.9.1 the tunnel is closed before the bytes are forwarded.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::Result;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_proxy::{ProxyHttp, Session};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::{sleep, timeout};

const ATTEMPTS: usize = 50;

struct Passthrough {
    upstream: SocketAddr,
}

#[async_trait]
impl ProxyHttp for Passthrough {
    type CTX = ();

    fn new_ctx(&self) {}

    async fn upstream_peer(&self, _: &mut Session, _: &mut ()) -> Result<Box<HttpPeer>> {
        Ok(Box::new(HttpPeer::new(self.upstream, false, String::new())))
    }

    async fn request_body_filter(
        &self,
        _: &mut Session,
        _: &mut Option<Bytes>,
        _: bool,
        _: &mut (),
    ) -> Result<()> {
        sleep(Duration::from_millis(10)).await;
        Ok(())
    }
}

/// Answers every upgrade request with `101` at once, then echoes what it receives.
async fn echo_upstream() -> SocketAddr {
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
                if stream.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
                let mut buf = [0_u8; 1024];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

/// One upgrade through the proxy; the error says how the tunnel failed.
async fn echo_once(proxy: SocketAddr) -> Result<(), String> {
    let mut client = common::connect(proxy).await;
    let request = "GET /chat HTTP/1.1\r\nHost: upstream.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n";
    client
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let (head, _) = common::read_head(&mut client).await;
    if !head.starts_with("HTTP/1.1 101") {
        return Err(format!("expected 101, got {head:?}"));
    }
    client
        .write_all(b"ping")
        .await
        .map_err(|e| format!("write after 101: {e}"))?;
    let mut echoed = [0_u8; 4];
    match timeout(Duration::from_secs(2), client.read_exact(&mut echoed)).await {
        Ok(Ok(_)) if &echoed == b"ping" => Ok(()),
        Ok(Ok(_)) => Err(format!("wrong echo {echoed:?}")),
        Ok(Err(e)) => Err(format!("tunnel closed after the 101: {e}")),
        Err(_) => Err("no echo within 2s".to_string()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn upgraded_tunnel_survives_a_101_before_the_end_task() {
    let upstream = echo_upstream().await;
    let proxy = common::start_proxy(Passthrough { upstream });
    let mut failed = Vec::new();
    for attempt in 1..=ATTEMPTS {
        if let Err(why) = echo_once(proxy).await {
            failed.push(format!("attempt {attempt}: {why}"));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {ATTEMPTS} tunnels failed; first: {}",
        failed.len(),
        failed[0]
    );
}
