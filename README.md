# w9pt

An embeddable 9P filesystem framework for pluggable storage backends.

`w9pt` translates stock `9P2000.L` requests into owned, backend-neutral filesystem and policy effects. `w9pt-storage` prepares immutable file content for caller-provided object stores; a future filesystem metadata layer will publish those references with inode and namespace state.

## Status

The Sans-I/O core implements framing, negotiation, the declared base/Linux operation matrix, tags, fids, out-of-order completions, cancellation, explicit shutdown, capabilities, and stable Linux errno replies. The storage crate implements bounded `raw` and sparse 32 KiB `block-split` content layouts over a runtime-neutral target contract. Transport, target SDK, and authoritative filesystem-metadata adapters remain separate future crates.

## Goals

- Make the `w9pt` crate a Sans-I/O protocol and filesystem core with no sockets, async runtime, threads, or storage SDK.
- Provide a library-first 9P server framework that applications embed in their own process.
- Keep transports replaceable: TCP, Unix sockets, WebSocket, virtio, or application-provided byte/message streams.
- Separate 9P framing and session state from filesystem semantics and persistent storage.
- Define a backend contract that can support S3 first and other storage systems later.
- Preserve real filesystem behavior—stable object identity, random I/O, atomic namespace operations, metadata, locks, and durability—where backend capabilities allow it.
- Negotiate capabilities and return explicit unsupported-operation errors instead of silently weakening semantics.

## Non-goals

- An opinionated standalone daemon, appliance, or control-plane runtime like ZeroFS.
- A mandatory CLI, configuration-file format, listener, scheduler, cache directory, or deployment topology.
- A direct `file path = S3 object key` mapping presented as a full filesystem.
- Claiming complete POSIX/Linux compatibility before the operation and failure semantics are tested.

## Architecture

```text
embedding application
  ├── feeds transport bytes/messages into w9pt
  ├── executes storage effects requested by w9pt
  └── sends frames emitted by w9pt
                  │ events/completions
                  ▼
          w9pt Sans-I/O core crate
       9P codec + session state machine
       filesystem semantics + typed effects
                  │ effects/results
          ┌───────┴──────────┐
          ▼                  ▼
 transport adapters    backend adapters
 TCP/Unix/WebSocket    S3 first, then others
```

The core performs no I/O. It consumes input and completion events, then emits transport frames, storage requests, cancellation requests, and lifecycle events for the embedding application to execute.

The host application decides whether a session is driven synchronously, by Tokio or another executor, through WebSocket, or behind an OS filesystem adapter. `w9pt` does not require a particular executable architecture.

## Driving a session

Create one independently owned `Session` per connection. The host supplies a stable `SessionId`, feeds arbitrary stream chunks with `receive_bytes` or one owned frame with `receive_frame`, and polls effects until quiescent.

```rust,no_run
use w9pt::{Effect, Session, SessionConfig, SessionContext, SessionId};

let mut session = Session::new(
    SessionConfig::default(),
    SessionContext::new(SessionId::new(42)),
)?;

session.receive_bytes(&[/* bytes supplied by the transport */])?;
while let Some(effect) = session.poll_effect() {
    match effect {
        Effect::SendFrame { bytes } => {
            // Write this complete frame after every earlier SendFrame from this session.
            let _ = bytes;
        }
        Effect::Filesystem { operation_id, request } => {
            // Execute owned semantic work, then call session.complete(...) exactly once.
            let _ = (operation_id, request);
        }
        Effect::Policy { operation_id, request } => {
            // Resolve authentication/export policy, then complete exactly once.
            let _ = (operation_id, request);
        }
        Effect::Cancel { operation_id, kind } => {
            // Best effort only. The original operation still needs one terminal completion.
            let _ = (operation_id, kind);
        }
        Effect::CloseSession { reason } => {
            // Stop transport input/output as appropriate for the host.
            let _ = reason;
        }
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Effects own all their payloads. A host may schedule filesystem and policy work in any order and may return unrelated completions in any order. Route work globally by `(SessionId, OperationId)` and return each emitted filesystem/policy operation exactly one matching terminal result or terminal cancellation. Unknown, duplicate, stale, and wrong-kind completions are errors and do not mutate another request.

`SendFrame` effects are complete response frames. They are emitted in the only transport order the host must preserve for that session; unrelated tags may finish in completion order rather than request-arrival order.

## Cancellation and shutdown

`Tflush` suppresses the old protocol reply and requests best-effort cancellation. `Rflush` waits for the old external work to become terminal, ensuring no old reply follows it. A successful mutation returned after a flush may already be committed: flush is never transaction rollback.

Call `Session::begin_close` when the host closes a connection. New ordinary input is rejected; the session emits cancellation and fid-release work. Continue polling and completing it until `SessionStatus::Drained`. `Drop` performs no hidden I/O. Dropping before drain is an explicit choice to abandon remaining backend live state.

## Export capabilities

Attach policy returns the principal, export ID, root object/QID, and `CapabilitySet`. Capabilities are enforced promises, not hints. Operations are rejected with `EOPNOTSUPP` before backend work unless the export declares both the operation and required guarantees such as atomic authorization/namespace mutation, stable identity, positioned I/O, open-unlinked lifetime, durability, xattrs, or cross-session locks.

The backend must coordinate stable identities, namespace transactions, open handles, permissions, and locks across independently owned sessions. Protocol code never exposes S3 keys, extents, transactions, or other storage-engine details.

## Supported protocol surface

The core intentionally handles `version`, `auth`, `attach`, `flush`, `walk`, `clunk`, `lopen`, `lcreate`, `mkdir`, `mknod`, `symlink`, `read`, `write`, `readdir`, `fsync`, `statfs`, `getattr`, `setattr`, `readlink`, `rename`, `renameat`, `remove`, `unlinkat`, `link`, `xattrwalk`, `xattrcreate`, `lock`, and `getlock`, plus `Rlerror`.

Legacy dialect negotiation, obsolete base `open`/`create`/`stat`/`wstat`, private reconnect/session migration, universal POSIX emulation, transports, runtimes, clocks, authentication mechanisms, and storage implementations are intentionally outside this crate.

## Rust I/O compatibility

Separate adapters can expose opened w9pt files through `std::io::Read`, `Write`, and `Seek`, allowing generic byte-stream libraries to work normally. Tokio/futures traits can be implemented in optional runtime crates.

Concrete `std::fs` functions are not pluggable: `std::fs::File`, `Metadata`, and `ReadDir` represent the host operating system filesystem. Code using `std::fs::read("path")` can access w9pt only after a FUSE/9P-style OS mount. In-process users will use w9pt's own filesystem API and optional `std::io` handle adapters.

## Immutable content repository

`w9pt-storage` is generic over bounded exact object reads, atomic immutable creation, and opaque-version compare-and-swap. It does not depend on an S3 SDK or async runtime. Its persisted manifests record the selected layout independently from representation: version 1 uses BLAKE3-256 over canonical plaintext, identity encoding, and no encryption.

The `raw` method stores one complete bounded file payload. A partial mutation verifies and rewrites that complete payload. The `block-split` method stores sparse, file-relative 32 KiB canonical blocks; missing and all-zero blocks are holes, the manifest owns logical EOF, and a partial final block is zero-padded before hashing.

Preparation and publication are deliberately separate. Payloads become durable first, then the immutable manifest; `PreparedContent` binds the caller's mutation ID, base content identity, deterministic operation fingerprint, and attempt into collision-safe immutable keys. Its `ContentRef` can be reconstructed and validated by a future authoritative metadata adapter after restart. `ObjectHeadPublisher` is a standalone single-key CAS publisher for tests and non-clustered use, not a second authority in clustered filesystem operation. Ambiguous CAS readback never rebases unless failure to commit is definitive.

Version 1 is write-through. Successful target puts and publication CAS operations are durable under the target contract, so `sync_content` has no write-back queue to flush. This provides content durability only: it does not claim durable inode or namespace metadata, atomic namespace operations, open-unlinked handling, cross-session locks, append serialization, garbage collection, compression, or encryption. Packed objects, paged manifests, codecs, ciphers, and GC require later versioned changes.

## First backend: S3

The planned S3 backend will use an opaque filesystem layout rather than one S3 object per visible file. The current research direction is:

- stable inode and directory metadata;
- fixed-size logical extents for random writes;
- immutable packed segment objects for efficient S3 access;
- range reads and local caching;
- data-before-metadata durability ordering;
- conditional writes for publication and writer fencing;
- sparse holes, copy-on-write updates, and garbage collection.

## Research

- [9P with an S3 backend](.dev/research/9p-s3-filesystem.md)
- [Sans-I/O core and Rust filesystem interoperability](.dev/research/sans-io-core.md)
- [File transfer versus filesystem protocols](.dev/research/file-transfer-vs-filesystem-protocols.md)
- [How NFS works](.dev/research/nfs.md)
- [SteamPipe chunked content distribution](.dev/research/steampipe-content-system.md)

## License

Apache-2.0. Any implementation inspired by AGPL projects must be independently written or used under separately compatible terms.
