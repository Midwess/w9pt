# Add WebSocket 9P Integration Profile

Status: approved

## Summary

Add a sibling integration profile that runs an application-owned HTTP server,
upgrades `/9p` requests to WebSocket, and carries one complete 9P frame in each
binary WebSocket message. The profile reuses the existing minimal SeaweedFS S3
filesystem fixture and verifies the complete negotiate, attach, create, write,
reopen, and read path over WebSocket.

## Motivation

The existing fixture demonstrates only stream framing over raw TCP. The core
also exposes `Session::receive_frame` specifically for message transports, but
there is no executable or end-to-end evidence showing how an HTTP/WebSocket
host should preserve 9P frame boundaries, enforce bounded input, route effects,
and return response frames. A dedicated profile closes that transport-embedding
gap without adding an HTTP runtime or WebSocket dependency to the core crates.

## Scope

### In scope

- Rename the standalone integration-test package so it is not TCP-specific.
- Add an Axum HTTP router with a lightweight `/healthz` endpoint and a `/9p`
  WebSocket upgrade endpoint.
- Require the `9p` WebSocket subprotocol and accept only bounded binary data
  messages as complete 9P frames.
- Allocate an independent `w9pt::Session` for every upgraded connection.
- Emit every `Effect::SendFrame` as one binary WebSocket message and complete
  application policy/filesystem effects through the existing fixture.
- Close the WebSocket on text input, malformed 9P input, or a core-requested
  session close while preserving WebSocket control-frame handling.
- Add a standalone WebSocket server binary and an end-to-end integration test
  backed by the digest-pinned SeaweedFS service.
- Run the new profile from the local integration runner and CI with its own
  required-mode gate and private object prefix.
- Share transport-neutral 9P test-client encoding and lifecycle assertions
  between the TCP and WebSocket integration tests.
- Document the wire contract and the fixture's non-production limits.

### Out of scope

- Adding HTTP, WebSocket, Axum, Tokio, or S3 dependencies to `w9pt` or another
  root workspace crate.
- TLS/WSS termination, browser authentication, Origin policy, compression,
  proxies, load balancing, reconnection, or transparent session migration.
- Persisting namespace, open-handle, or session state across process loss.
- Replacing the future production transport gateway or filesystem semantic
  engine.
- Advertising new filesystem, durability, cancellation, or migration
  capabilities.

## Success criteria

- A normal HTTP request receives a healthy bounded response from `/healthz`.
- A WebSocket client offering `9p` receives HTTP 101 and the selected `9p`
  subprotocol at `/9p`.
- The end-to-end client completes the same 9P2000.L file lifecycle as the TCP
  profile, with every request and response carried in one binary message.
- The file bytes observed through 9P exactly match the bytes stored through the
  SeaweedFS S3 API.
- Missing subprotocol and text-message inputs are rejected deterministically.
- Local runner, CI, formatting, Clippy, tests, rustdoc, dependency metadata,
  audit, and teardown checks remain green.

## Dependencies

- Applied `complete-sans-io-core` message-transport API.
- Existing standalone SeaweedFS integration fixture and Compose service.
- Axum 0.8 WebSocket support and the matching Tokio Tungstenite client.

## Risks

| Risk | Mitigation |
| --- | --- |
| WebSocket messages are treated as arbitrary stream chunks | Route each binary message through `Session::receive_frame` and test one-message/one-frame request and response boundaries. |
| The HTTP profile weakens resource limits | Configure WebSocket frame and message maxima to the core's 1 MiB default and keep request processing sequential for backpressure. |
| A transport demo is mistaken for production gateway support | Keep it in the standalone `test/` workspace and document omitted TLS, authentication, persistence, recovery, and proxy policy. |
| New runtime dependencies leak into the core | Add dependencies only to `test/Cargo.toml` and retain the root dependency-isolation gates. |
| Multiple clients reuse a session identity | Allocate a checked monotonically increasing session ID per upgrade and test more than one connection. |

