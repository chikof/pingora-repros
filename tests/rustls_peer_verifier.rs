//! A rustls upstream cannot be verified by a SPIFFE ID, only by chain plus hostname or by chain
//! alone.
//!
//! Two upstreams have certificates from one CA. Their only identity is a URI SAN:
//! `spiffe://example.test/a` and `spiffe://example.test/b`. The proxy wants `/a`. It must connect
//! to the first upstream and refuse the second. Stock pingora-core 0.9.1 offers `verify_cert` and
//! `verify_hostname` and nothing else for rustls. The best a stock peer can do is chain-only
//! (`verify_hostname = false`), which also accepts the second upstream, so the test fails with
//! `/b` answered 200. With the patch the peer brings its own verifier (`PeerOptions::
//! server_cert_verifier`), which is the `patched` build of `peer()` below.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use pingora_core::Result;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::utils::tls::{WrappedX509, parse_x509};
use pingora_proxy::{ProxyHttp, Session};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const WANTED: &str = "spiffe://example.test/a";

struct Ca {
    der: CertificateDer<'static>,
    issuer: Issuer<'static, KeyPair>,
}

fn ca() -> Ca {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let key = KeyPair::generate().expect("key");
    let der = params.self_signed(&key).expect("ca").der().clone();
    Ca {
        der,
        issuer: Issuer::new(params, key),
    }
}

/// A server certificate whose only name is the URI SAN `id`.
fn svid(ca: &Ca, id: &str) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = vec![SanType::URI(id.try_into().expect("uri"))];
    let key = KeyPair::generate().expect("key");
    let cert = params.signed_by(&key, &ca.issuer).expect("svid");
    let key = PrivatePkcs8KeyDer::from(key.serialize_der());
    (cert.der().clone(), key.into())
}

/// A TLS upstream with the certificate for `id` that answers every request with `200 ok`.
async fn upstream(ca: &Ca, id: &str) -> SocketAddr {
    let (cert, key) = svid(ca, id);
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("versions")
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .expect("server config");
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let _ = common::read_head(&mut tls).await;
                let reply = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                let _ = tls.write_all(reply.as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    addr
}

/// The peer for `addr`: trusts the CA and wants the server to be `WANTED`.
#[cfg(not(feature = "patched"))]
fn peer(addr: SocketAddr, ca: &CertificateDer<'static>) -> HttpPeer {
    let mut peer = HttpPeer::new(addr, true, "upstream.test".to_string());
    peer.options.ca = Some(roots(ca));
    // No stock option looks at a URI SAN. A SPIFFE server has no DNS name, so the hostname check
    // has to go, and what is left is the chain.
    peer.options.verify_hostname = false;
    peer
}

#[cfg(feature = "patched")]
fn peer(addr: SocketAddr, ca: &CertificateDer<'static>) -> HttpPeer {
    use std::hash::{DefaultHasher, Hash, Hasher};

    let mut peer = HttpPeer::new(addr, true, "upstream.test".to_string());
    peer.options.ca = Some(roots(ca));
    peer.options.server_cert_verifier = Some(Arc::new(spiffe::Verifier::new(ca, WANTED)));
    // The reuse hash does not cover the verifier, so what it decides on goes into the group key.
    let mut hasher = DefaultHasher::new();
    WANTED.hash(&mut hasher);
    peer.group_key = hasher.finish();
    peer
}

fn roots(ca: &CertificateDer<'static>) -> Arc<[WrappedX509]> {
    Arc::from(vec![WrappedX509::new(ca.as_ref().to_vec(), parse_x509)])
}

#[cfg(feature = "patched")]
mod spiffe {
    use std::sync::Arc;

    use rustls::client::WebPkiServerVerifier;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{CertificateError, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
    use x509_parser::extensions::GeneralName;

    /// Chain to the CA, then the leaf's URI SAN must be `expected`. The DNS name is ignored.
    #[derive(Debug)]
    pub struct Verifier {
        chain: Arc<WebPkiServerVerifier>,
        expected: &'static str,
    }

    impl Verifier {
        pub fn new(ca: &CertificateDer<'static>, expected: &'static str) -> Self {
            let mut roots = RootCertStore::empty();
            roots.add(ca.clone()).expect("root");
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let chain = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .expect("chain verifier");
            Self { chain, expected }
        }
    }

    fn uri_sans(der: &[u8]) -> Vec<String> {
        let Ok((_, cert)) = x509_parser::parse_x509_certificate(der) else {
            return Vec::new();
        };
        let Ok(Some(san)) = cert.subject_alternative_name() else {
            return Vec::new();
        };
        san.value
            .general_names
            .iter()
            .filter_map(|name| match name {
                GeneralName::URI(uri) => Some((*uri).to_string()),
                _ => None,
            })
            .collect()
    }

    impl ServerCertVerifier for Verifier {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            intermediates: &[CertificateDer<'_>],
            server_name: &ServerName<'_>,
            ocsp: &[u8],
            now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            // The chain is checked before the name, so a name error means the chain was good.
            match self
                .chain
                .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
            {
                Ok(_)
                | Err(Error::InvalidCertificate(
                    CertificateError::NotValidForName
                    | CertificateError::NotValidForNameContext { .. },
                )) => {}
                Err(err) => return Err(err),
            }
            if uri_sans(end_entity.as_ref()) == [self.expected] {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(Error::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure,
                ))
            }
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            self.chain.verify_tls12_signature(message, cert, dss)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            self.chain.verify_tls13_signature(message, cert, dss)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.chain.supported_verify_schemes()
        }
    }
}

struct ByPath {
    a: SocketAddr,
    b: SocketAddr,
    ca: CertificateDer<'static>,
}

#[async_trait]
impl ProxyHttp for ByPath {
    type CTX = ();

    fn new_ctx(&self) {}

    async fn upstream_peer(&self, session: &mut Session, _: &mut ()) -> Result<Box<HttpPeer>> {
        let addr = if session.req_header().uri.path() == "/a" {
            self.a
        } else {
            self.b
        };
        Ok(Box::new(peer(addr, &self.ca)))
    }
}

async fn status(proxy: SocketAddr, path: &str) -> u16 {
    use tokio::io::AsyncReadExt;

    let mut client = common::connect(proxy).await;
    let request = format!("GET {path} HTTP/1.1\r\nHost: x.test\r\nConnection: close\r\n\r\n");
    client.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    client.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw);
    text.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status line")
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_wanted_spiffe_id_is_accepted() {
    let ca = ca();
    let a = upstream(&ca, WANTED).await;
    let b = upstream(&ca, "spiffe://example.test/b").await;
    let proxy = common::start_proxy(ByPath {
        a,
        b,
        ca: ca.der.clone(),
    });
    assert_eq!(
        status(proxy, "/a").await,
        200,
        "the upstream with the wanted ID was refused"
    );
    assert_eq!(
        status(proxy, "/b").await,
        502,
        "the upstream with spiffe://example.test/b was accepted for {WANTED}"
    );
}
