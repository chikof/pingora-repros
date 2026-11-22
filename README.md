# pingora-repros

Four problems in Pingora 0.9.1 that I hit in a reverse proxy built on it, each with a test that fails on the stock crates and passes with a patch.

I carry these four patches as local copies of `pingora-proxy` and `pingora-core`. This repo pulls them out of my proxy so they can be run and read on their own.

Status against `cloudflare/pingora` main at `[3f87e92](https://github.com/cloudflare/pingora/commit/3f87e92)`, 67 commits after 0.9.1:

| # | Problem | Upstream |
|---|---|---|
| 1 | A `101` read before the request's end-of-body task closes an upgraded tunnel | Reported as [#946] and [#1050]. Open PRs: [#947], [#1051], [#1016]. |
| 2 | No way to set an upgraded tunnel's read timeout or end it | Not reported, not fixed on main. |
| 3 | An HTTP/2 response that ends in trailers never gives `response_body_filter` its end of stream | Not reported, not fixed on main. |
| 4 | No per-peer server certificate verifier for rustls | Not fixed for rustls. Main's `cb16c87` adds `PeerOptions::upstream_tls_verify_callback`, for the OpenSSL and BoringSSL backends only. |

## Running

You need Rust 1.99.0 (`rust-toolchain.toml`) and cmake, because `libz-ng-sys` builds zlib-ng. With Nix:

```
devenv shell
scripts/run.sh stock        # pingora-proxy and pingora-core 0.9.1 from crates.io
scripts/run.sh patched      # the same crates with patches/*.patch applied
scripts/run.sh stock 3      # one problem: 1 to 4, or the test file name
```

Without Nix, run the same scripts from a checkout; they only need cargo, patch and bash.

`patched` copies the two crates from the cargo registry into `target/patched-src/`, applies the patches and points cargo at the copies with `--config 'patch.crates-io...'`. `Cargo.toml` is not edited. `Cargo.lock` is put back when the run ends. `scripts/check-patches.sh` applies every patch alone, and all four in order, to fresh registry copies.

Each test starts a Pingora proxy inside the test process on `127.0.0.1`, plus a small upstream and a raw-socket or `h2` client. One test file runs alone: `cargo test --test tunnel_timeout`.

Where stock Pingora has no API for the fix (problems 2 and 4 and the hook of 3), the test uses the new API only under `--features patched`. The stock build runs the same scenario with the best stock configuration and fails on the symptom.

Results on this repo's lockfile, Linux x86_64, debug build:

| Test | Stock 0.9.1 | Patched |
|---|---|---|
| `upgrade_101_race` | fails: `50 of 50 tunnels failed; first: attempt 1: tunnel closed after the 101: early eof` | passes |
| `tunnel_timeout` | fails: `the idle tunnel was still open after 5s; its read timeout stayed at 30s` | passes (2 tests) |
| `h2_trailers_end_of_stream` | fails: `the filter never saw end_of_stream, so it never released the body` (`left: ""`, `right: "hello world"`) | passes |
| `rustls_peer_verifier` | fails: `the upstream with spiffe://example.test/b was accepted for spiffe://example.test/a` (`left: 200`, `right: 502`) | passes |

## 1. An upgraded tunnel is closed when the 101 wins a race

An upgrade `GET` has no body. `proxy_handle_downstream` queues an empty `Body` task marked as the end for it before its loop (`proxy_h1.rs:485-497`). `proxy_handle_upstream` waits on `read_response_task()` and `rx.recv()` in one `tokio::select!` (`proxy_h1.rs:267`, `:326`).

If the end task is taken first, `request_done` becomes `true`, and the `101` then sets it back to `false` (`:272-278`). If the `101` is taken first, `request_done` is already `false`. The end task then arrives on an upgraded connection and `if request_done && client_session.was_upgraded() { response_done = true }` (`:331-332`) ends the loop. The tunnel closes before the first client frame is forwarded.

The `101` goes first when it is readable when the loop starts, and `select!` picks a ready branch at random, or when the end task is queued late. My proxy lost about 1 in 10 WebSocket connections to a local upstream this way.

The patch (`patches/0001-h1-upgrade-101-race.patch`) tells a plain `Body` task after the upgrade, which is the original request's end, from `UpgradedBody`. Only `UpgradedBody` or a closed pipe ends an upgraded connection.

`tests/upgrade_101_race.rs` delays the end task. Its `request_body_filter` sleeps 10ms and the upstream answers `101` at once. The `101` is then always first. 50 upgrades fail on stock, none with the patch. I could not reproduce the natural race here. An echo upstream that answers right after reading the request, on tokio or on a spinning thread, lost 0 of 1000 tunnels on stock in each of three runs. The 10% figure comes from my proxy's own tests against a local echo server.

Upstream: reported as [#946] and [#1050], PRs [#947], [#1051] and [#1016] are open.

## 2. An upgraded tunnel's timeout and lifetime cannot be set

After a `101`, the upstream read timeout of an HTTP/1 tunnel stays the peer's `read_timeout` (`proxy_h1.rs:43`), which was chosen for the wait for the response head. A proxy that wants a separate idle timeout for tunnels, a maximum lifetime, or to close a tunnel whose server was removed has no way to: `client_session` lives inside `proxy_handle_upstream`, and no `ProxyHttp` callback can reach it or stop the loop.

The patch (`patches/0002-proxy-tunnel-hook.patch`) adds `ProxyHttp::tunnel(&self, &Session, &mut CTX) -> Option<Tunnel>`, called once per upgrade request. `Tunnel { read_timeout, cancel }` sets the read timeout that applies once the upgrade succeeded, and `cancel.notify_one()` ends the tunnel. The default `None` keeps today's behaviour.

`tests/tunnel_timeout.rs` sets the peer's `read_timeout` to 30s and expects an idle tunnel to close within 5s. On stock the tunnel is still open after 5s. With the patch the hook sets 500ms and the tunnel closes. A second test, built only with `--features patched`, cancels an open tunnel.

Upstream: not reported, not fixed on main.

## 3. HTTP/2 trailers hide the end of the body

When an HTTP/2 upstream response ends with a TRAILERS frame, the last DATA frame does not carry END_STREAM, so `response_body_filter` is never called with `end_of_stream = true`. A filter that holds bytes until the end, such as a buffering or compressing one, loses its tail. `response_trailer_filter` cannot help. It can only turn trailers into body bytes, which drops the trailers (`proxy_h2.rs:993-1021`).

The patch (`patches/0003-h2-response-body-before-trailers.patch`) adds `ProxyHttp::response_body_before_trailers`. `process_upstream_tasks_h2` calls it once, before `response_trailer_filter`, when the response ends in non-empty trailers. What it returns is written as body data ahead of the trailers, which are still sent as trailers. The default `None` keeps today's behaviour. Calling `response_body_filter` with `end_of_stream = true` and an empty chunk would work too.

`tests/h2_trailers_end_of_stream.rs` has an `h2c` upstream that sends two DATA frames and trailers. The proxy's filter holds the body until `end_of_stream`. On stock the client gets an empty body. With the patch the hook releases `hello world`.

Upstream: not reported, not fixed on main.

## 4. rustls peers cannot bring their own certificate verifier

The rustls connector picks the verifier from `verify_cert` and `verify_hostname`: chain plus hostname, chain only, or nothing (`connectors/tls/rustls/mod.rs:227-258`). A SPIFFE server is identified by a URI SAN, not a DNS name, so neither full check can judge it. With OpenSSL a caller can inspect the peer in `upstream_tls_handshake_complete_hook`, but that hook exists only under `openssl_derived`.

The patch (`patches/0004-rustls-server-cert-verifier.patch`) adds `PeerOptions::server_cert_verifier: Option<Arc<dyn ServerCertVerifier>>` under `cfg(feature = "rustls")`. The connector uses it in place of the chosen verifier. `verify_cert` and `verify_hostname` are not consulted. The connection reuse hash does not cover the verifier, so a caller must put what the verifier decides on into `group_key`. The default `None` keeps today's behaviour.

`tests/rustls_peer_verifier.rs` has two TLS upstreams whose certificates come from one CA and carry only the URI SANs `spiffe://example.test/a` and `.../b`. The proxy wants `a` for both. On stock the best configuration is chain-only, which accepts `b` as well. With the patch a verifier checks the chain and the URI SAN, and `b` gets a 502.

Upstream: not fixed for rustls. Main's `cb16c87` adds `PeerOptions::upstream_tls_verify_callback` for the OpenSSL and BoringSSL backends only.

## License

The tests, scripts and docs are `Apache-2.0 OR MIT`. `patches/` is Apache-2.0, Copyright Cloudflare, Inc.: the patches carry code and context lines from cloudflare/pingora, which is Apache-2.0 only. The licence text is in `patches/LICENSE`.

[#946]: https://github.com/cloudflare/pingora/issues/946
[#947]: https://github.com/cloudflare/pingora/pull/947
[#1016]: https://github.com/cloudflare/pingora/pull/1016
[#1050]: https://github.com/cloudflare/pingora/issues/1050
[#1051]: https://github.com/cloudflare/pingora/pull/1051
