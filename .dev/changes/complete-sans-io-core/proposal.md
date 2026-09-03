# Complete the Sans-I/O Core

Status: approved

## Summary

Replace the generated `w9pt` library template with a complete, dependency-light Sans-I/O framework for serving stock `9P2000.L`. The crate will decode requests, maintain connection-scoped protocol state, translate requests into owned backend-neutral filesystem and policy effects, accept their completions in any order, and encode replies without performing transport, storage, clock, executor, or operating-system I/O.

For this proposal, “complete core” means that every stock `9P2000.L` request in the declared operation matrix has wire support and deterministic session behavior. It does not mean universal POSIX compatibility: an attached export advertises its capabilities, and unsupported semantics fail explicitly.

## Motivation

The crate currently contains only Cargo's generated `add` function. The research established a library-first Sans-I/O direction, but the public ownership model, shared-state boundary, cancellation rules, capabilities, and error domains are still open. These contracts must be settled before an S3 backend or transport adapter can be implemented without coupling protocol code to a runtime or storage technology.

Completing the core first provides:

- one canonical API for TCP, Unix socket, WebSocket, virtio, and test drivers;
- one backend-neutral contract for S3 and future filesystem engines;
- deterministic tests for framing, tag/fid state, out-of-order completion, and `Tflush` races;
- explicit semantic and durability requirements instead of accidental S3 behavior;
- a clean Apache-2.0 implementation boundary independent of ZeroFS's AGPL code.

## Goals

- Support exact `9P2000.L` negotiation and the stock base/Linux request set listed in the delta specification.
- Provide incremental stream input and complete-frame input through one checked codec.
- Model transport output, filesystem work, policy decisions, cancellation, and closure as owned typed effects and completions.
- Keep one independently owned `Session` per transport connection, with no singleton or mandatory shared runtime object.
- Correlate concurrent requests by tag and opaque operation ID while accepting completions in any order.
- Specify fid lifecycle, partial walks, attach/export context, resource cleanup, and session shutdown.
- Define `Tflush` as reply cancellation with best-effort backend cancellation, never as transaction rollback.
- Define backend-neutral operations, stable error mapping, capability promises, atomicity, and durability contracts.
- Bound all attacker-controlled input and queued state.
- Supply deterministic conformance tests, golden wire vectors, and fuzz/property-test entry points.
- Document a host driving loop and the obligations attached to every emitted effect.

## Scope

### In scope

- The public API and implementation under `crates/w9pt`.
- A Rust 2024, `std`-based, synchronous Sans-I/O state machine.
- 9P scalar types, flags, messages, little-endian codecs, and frame validation.
- Negotiation, tags, fids, QIDs, session context, outstanding operations, response queues, and explicit shutdown.
- Owned filesystem and policy request/result types with opaque handles.
- Capability-based operation dispatch and `Rlerror` mapping through project-defined Linux errno values.
- Full wire/dispatch coverage for the declared stock `9P2000.L` operation matrix.
- A deterministic model driver used only by tests.
- Crate documentation and examples that perform no real I/O in the core.

### Out of scope

- An S3 backend, extent/segment storage, metadata database, caching, garbage collection, or recovery implementation.
- TCP, Unix socket, WebSocket, HTTP, Tokio, FUSE, NFS, CSI, or other runtime adapters.
- A daemon, CLI, listener, deployment topology, load balancer, or mandatory global server.
- Blocking `std::io::{Read, Write, Seek}` adapters or transparent `std::fs` integration.
- A private reconnect/session-migration dialect, stateless WebSocket-node recovery, or retry ledger.
- `no_std`, zero-copy borrowed completions, and public API stability beyond the crate's pre-1.0 compatibility policy.
- Semantics absent from stock `9P2000.L`, including universal `renameat2`, `fallocate`, reflink, lease, notification, and arbitrary `ioctl` support.
- Copying, translating, or adapting ZeroFS source, tests, internal formats, or distinctive implementation structure.

## Acceptance Criteria

- `cargo test --workspace --all-targets` passes with the generated template removed.
- The normal dependency graph contains no async runtime, network client, object-store SDK, filesystem engine, or platform `libc` dependency.
- The codec has independent golden vectors and rejects malformed, truncated, oversized, and dialect-invalid frames without panicking.
- Every operation in the matrix decodes and either emits the correct typed effect or returns a capability/error response.
- Session tests cover duplicate tags/fids, partial walks, out-of-order completions, late/duplicate/wrong-type completions, cleanup, and the specified `Tflush` races.
- Configured limits bound input buffering, queued output/effects, in-flight tags, fids, walk elements, strings, and pending write bytes.
- Public documentation explains effect ownership, completion obligations, output ordering, shutdown, capability guarantees, and exclusions.

## Risks

- Incorrect wire layouts or Linux flag/errno values could interoperate with the crate's own tests but fail against real clients; independent protocol vectors are required.
- Permission checks separated from a mutation would introduce time-of-check/time-of-use races; filesystem requests must carry principal/export context and require atomic authorization at the semantic boundary.
- `Tflush` cannot undo a mutation that already committed. The API must suppress the old reply and make rollback explicitly out of contract.
- Dropping a session cannot execute release effects. Hosts must drive explicit shutdown to completion or knowingly abandon backend live state.
- Owned buffers are simpler and safe across arbitrary completion order but may copy more data than a later optimized API.
- “Complete” may be mistaken for full POSIX support unless the operation/capability matrix remains prominent.
- Detailed prior ZeroFS research creates contamination risk; implementation must use public protocol documents and original tests rather than AGPL source.

## Dependencies

- Rust 2024 and the standard library are the initial platform baseline.
- No normal third-party dependency is required by the proposed design; a narrowly justified codec utility may be proposed separately if implementation evidence warrants it.
- Property/fuzz tooling may be added as development-only infrastructure, but golden tests must not depend only on encode/decode round trips.
- The implementation depends on the host supplying authenticated session context, policy completions, filesystem completions, and ordered delivery of emitted transport frames.
- Future S3 and transport crates will depend on this crate; this crate must not depend on them.

