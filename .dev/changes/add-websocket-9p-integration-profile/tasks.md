# Tasks: add-websocket-9p-integration-profile

## Progress: [17/17]

## 1. Contract and package

- [x] 1.1 Specify the HTTP routes, WebSocket subprotocol, frame mapping, limits, and failure behavior.
- [x] 1.2 Rename the standalone test package and define explicit TCP and WebSocket binaries.
- [x] 1.3 Add current, pinned HTTP/WebSocket dependencies only to the standalone test workspace.

## 2. WebSocket server

- [x] 2.1 Add cloneable HTTP router state with checked per-upgrade session ID allocation.
- [x] 2.2 Add `/healthz` and subprotocol-gated `/9p` routes.
- [x] 2.3 Feed bounded binary messages to `Session::receive_frame` and return one binary message per `SendFrame`.
- [x] 2.4 Handle control frames, text rejection, malformed input, peer closure, core closure, and cleanup.
- [x] 2.5 Add a standalone HTTP/WebSocket server binary with caller-owned address and S3 configuration.

## 3. Integration tests

- [x] 3.1 Extract shared transport-neutral 9P client lifecycle helpers from the TCP test.
- [x] 3.2 Keep the existing TCP integration profile green after the package rename/refactor.
- [x] 3.3 Add bounded HTTP health and missing-subprotocol assertions.
- [x] 3.4 Add the complete binary WebSocket 9P lifecycle and direct-S3 verification.
- [x] 3.5 Add deterministic text-message rejection and independent-session coverage.

## 4. Automation and documentation

- [x] 4.1 Run the new required profile with an isolated prefix in `test/run-integration.sh`.
- [x] 4.2 Add a separate required WebSocket step and environment to CI.
- [x] 4.3 Update integration and project documentation with the transport contract and limitations.
- [x] 4.4 Run formatting, Clippy, tests, rustdoc, metadata/license, audit, script, Compose, diff, and live integration gates.

## Notes

The user directly approved implementation by asking to add the profile. This
change intentionally demonstrates application composition; it does not add a
production transport crate or durable session gateway.

Validation evidence is recorded in `analysis.md`; all required local and live
Compose gates passed on 2026-09-06.
