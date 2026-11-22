//! An HTTP/2 upstream response that ends in trailers never gives `response_body_filter` its end
//! of stream.
//!
//! The upstream sends two DATA frames without END_STREAM, then trailers. The proxy's filter holds
//! the body until `end_of_stream`, as a buffering or compressing filter does, and releases it
//! then. On stock pingora-proxy 0.9.1 the end never comes and the client receives an empty body.
//! With the patch `ProxyHttp::response_body_before_trailers` lets the filter release what it
//! holds; that hook is left out of the stock build, so the test compiles against both.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Response};
use pingora_core::Result;
use pingora_core::protocols::ALPN;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_proxy::{ProxyHttp, Session};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

struct Buffering {
    upstream: SocketAddr,
}

#[async_trait]
impl ProxyHttp for Buffering {
    type CTX = Vec<u8>;

    fn new_ctx(&self) -> Vec<u8> {
        Vec::new()
    }

    async fn upstream_peer(&self, _: &mut Session, _: &mut Vec<u8>) -> Result<Box<HttpPeer>> {
        let mut peer = HttpPeer::new(self.upstream, false, String::new());
        peer.options.alpn = ALPN::H2;
        Ok(Box::new(peer))
    }

    fn response_body_filter(
        &self,
        _: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        held: &mut Vec<u8>,
    ) -> Result<Option<Duration>> {
        if let Some(chunk) = body.take() {
            held.extend_from_slice(&chunk);
        }
        if end_of_stream {
            *body = Some(Bytes::from(std::mem::take(held)));
        }
        Ok(None)
    }

    #[cfg(feature = "patched")]
    fn response_body_before_trailers(
        &self,
        _: &mut Session,
        held: &mut Vec<u8>,
    ) -> Result<Option<Bytes>> {
        Ok(Some(Bytes::from(std::mem::take(held))))
    }
}

/// A cleartext HTTP/2 server: `hello ` and `world` as DATA, then the trailer `x-sum: 11`.
async fn h2_upstream_with_trailers() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Ok(mut conn) = h2::server::handshake(tcp).await else {
                    return;
                };
                while let Some(Ok((_request, mut respond))) = conn.accept().await {
                    tokio::spawn(async move {
                        let response = Response::builder().status(200).body(()).expect("response");
                        let mut stream = respond.send_response(response, false).expect("headers");
                        stream
                            .send_data(Bytes::from_static(b"hello "), false)
                            .expect("data");
                        stream
                            .send_data(Bytes::from_static(b"world"), false)
                            .expect("data");
                        let mut trailers = HeaderMap::new();
                        trailers.insert("x-sum", HeaderValue::from_static("11"));
                        stream.send_trailers(trailers).expect("trailers");
                    });
                }
            });
        }
    });
    addr
}

/// Joins the chunks of a `Transfer-Encoding: chunked` body; stops at the zero chunk.
fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = raw.windows(2).position(|w| w == b"\r\n") {
        let size = std::str::from_utf8(&raw[..line_end]).expect("size line");
        let size =
            usize::from_str_radix(size.split(';').next().expect("size").trim(), 16).expect("hex");
        if size == 0 {
            break;
        }
        let start = line_end + 2;
        out.extend_from_slice(&raw[start..start + size]);
        raw = &raw[start + size + 2..];
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn buffered_body_is_released_before_trailers() {
    let upstream = h2_upstream_with_trailers().await;
    let proxy = common::start_proxy(Buffering { upstream });
    let mut client = common::connect(proxy).await;
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: upstream.test\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut raw = Vec::new();
    timeout(Duration::from_secs(5), client.read_to_end(&mut raw))
        .await
        .expect("the response did not finish within 5s")
        .expect("read");
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response head")
        + 4;
    let head = String::from_utf8_lossy(&raw[..split]);
    assert!(head.starts_with("HTTP/1.1 200"), "unexpected head {head:?}");
    let body = dechunk(&raw[split..]);
    assert_eq!(
        String::from_utf8_lossy(&body),
        "hello world",
        "the filter never saw end_of_stream, so it never released the body"
    );
}
