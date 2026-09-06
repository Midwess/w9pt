# Implementation Analysis

## Outcome

The standalone integration package now provides both raw TCP and
HTTP/WebSocket 9P profiles over the existing minimal SeaweedFS-backed
application filesystem. The WebSocket profile uses Axum only in the separate
`test/` workspace and leaves every root workspace crate runtime- and
transport-neutral.

## Implemented behavior

- `GET /healthz` returns `204 No Content`.
- `/9p` rejects upgrades that do not offer `Sec-WebSocket-Protocol: 9p` and
  selects `9p` on success.
- Each successful upgrade receives a checked, monotonically increasing
  `SessionId` and a new independent `w9pt::Session`.
- Each binary WebSocket message is supplied as one owned complete frame through
  `Session::receive_frame`; each `SendFrame` effect is one binary response
  message.
- WebSocket messages and frames are bounded at 1 MiB, matching the current
  default core hard frame limit. Input is processed sequentially to preserve
  backpressure in the fixture.
- Ping/Pong control traffic remains transport control traffic. Peer close and
  disconnect report transport closure and drain filesystem/policy cleanup.
- Text data closes with code 1003 and malformed binary 9P closes with code
  1002.
- The standalone `w9pt-websocket-seaweedfs-test` binary accepts caller-owned
  listen and S3 configuration.
- TCP and WebSocket clients share one transport-neutral 9P lifecycle helper,
  which additionally validates that response length equals the complete
  transport message boundary.

## Dependency decision

The standalone workspace pins Axum 0.8.9 and Futures Util 0.3.34. It pins Tokio
Tungstenite 0.29.0 because that is Axum 0.8.9's WebSocket dependency line,
avoiding two Tungstenite versions. Tokio remains at the existing 1.53.1 pin and
adds only its `time` feature for the bounded integration test. The standalone
package now declares the repository's Apache-2.0 license metadata.

## Live evidence

`./test/run-integration.sh` passed against the exact SeaweedFS 4.42 and
PostgreSQL images. The WebSocket test observed HTTP health, missing-subprotocol
rejection, successful 101/`9p` negotiation, the full 9P2000.L file lifecycle,
exact direct-S3 readback, independent subsequent sessions, close code 1003 for
text, and close code 1002 for a four-byte malformed 9P frame. The existing S3
target/repository, PostgreSQL, and TCP profiles passed in the same run, and
Compose teardown left no scoped resources.

## Quality evidence

- Root and standalone workspace formatting passed.
- Root and standalone all-target tests passed.
- Root and standalone Clippy passed with warnings denied.
- Root and standalone rustdoc passed with warnings denied.
- Root and standalone dependency license metadata passed.
- Axum and Tokio Tungstenite are unreachable from the root workspace graph.
- The standalone lockfile RustSec audit passed.
- The root lockfile audit passed with the existing guarded unreachable
  `RUSTSEC-2026-0235` and `RUSTSEC-2023-0071` exceptions.
- Shell syntax, Compose validation, and `git diff --check` passed.

## Deliberate limits

This remains a development fixture. It does not provide TLS/WSS termination,
Origin/proxy/authentication policy, persistent namespace or session state,
gateway failover, reconnectable sessions, production filesystem semantics, or
SeaweedFS production qualification.

