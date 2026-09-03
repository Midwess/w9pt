# Design: Complete the Sans-I/O Core

## Context

`w9pt` must translate stateful `9P2000.L` traffic into filesystem semantics while remaining usable inside arbitrary runtimes. Removing only socket I/O is insufficient: directly calling an async storage trait would still force scheduling, lifetime, and SDK choices into the core. The design therefore makes both transport and filesystem work explicit effects.

The hardest boundary is shared filesystem state. Fids and tags belong to one connection, but stable inodes, namespace atomicity, open-unlinked objects, permissions, and locks cross connections. The core must express those requirements without installing a global runtime or assuming that one process owns all sessions.

## Decisions

### 1. `Session` is the primary state object

Each accepted connection gets an independently owned `Session`. It contains:

- validated configuration and host-supplied `SessionContext`;
- negotiated dialect and `msize`;
- bounded decoder and output/effect queues;
- fid table and per-fid protocol/open state;
- active tag table and operation-ID index;
- multi-stage request and flush state;
- terminal/closing state.

The host supplies a stable `SessionId`. An `OperationId` is opaque, monotonically allocated, and never reused within that session. Hosts use the pair to route completions when many sessions share one executor.

`Drop` does not perform cleanup work. `Session::begin_close` (exact name subject to implementation) stops new requests and emits cancellation/release operations. The host polls and completes them until the session reports it is drained. Abandoning a session before drain is an explicit host decision.

### 2. Shared semantics live behind the filesystem contract

The core does not own a global `Server`, `Arc<Mutex<_>>`, database, or lock service. Instead, filesystem requests include enough context for an implementation to coordinate all sessions:

```text
RequestContext
  session_id
  principal/export identity
  request identity
  opaque open/lock owner identity where needed
```

Opaque backend handles, rather than paths or S3 keys, represent resolved objects and open instances. The filesystem contract requires atomic authorization and mutation so the core never performs a policy check followed by a raceable backend update.

Authentication remains host-owned. `Tauth`, authentication-fid reads/writes, principal mapping, and attach/export selection are driven through policy effects. A host may reject `Tauth` and attach a transport-authenticated principal, or implement a protocol authentication exchange, without placing credentials or authentication I/O in the core.

The backend must implement or explicitly reject stable object identity, namespace transactions, open-unlinked lifetime, byte-range locks, and requested durability. This lets a single-threaded model engine, S3 engine, or distributed metadata service satisfy the same protocol layer.

### 3. Effects own their data

Effects and completions cannot borrow from `Session`, because hosts may queue them and complete them later in any order. Initial public payloads therefore own strings, vectors, attributes, and data buffers.

- Stream ingestion accepts `&[u8]` and copies only retained bytes.
- Complete-frame ingestion accepts an owned `Vec<u8>` and may decode it without an additional frame copy.
- Encoded responses are owned complete frames.
- Read and write data cross the filesystem boundary as owned bytes.

This choice favors a clear safe contract over early zero-copy optimization. A future compatible buffer abstraction needs evidence from benchmarks and is outside this proposal.

### 4. One checked codec serves both input modes

The incremental decoder reads the four-byte little-endian size, validates the minimum seven-byte header and configured maximum before allocation, and retains an incomplete tail. Each complete slice is decoded by the same message decoder used by `receive_frame`.

Complete-frame input requires the declared size to equal the supplied frame length. Stream input may contain zero, one, or many frames. Both paths enforce configured hard limits and, after negotiation, `msize`.

The codec performs checked arithmetic and explicit little-endian conversion. It never casts wire bytes to native Rust layouts. It distinguishes an incomplete stream from a malformed complete frame.

### 5. Pending operations have exact terminal rules

One client request may issue multiple sequential policy/filesystem effects. At most one effect for that request is active at a time unless a later operation explicitly documents parallel suboperations. The pending entry records the expected completion kind and continuation state.

Completions may arrive in any order across requests. A completion is accepted only once and only for the exact expected result kind. Unknown, stale, duplicate, and mismatched completions return `CompletionError` and cannot complete a newer request.

Responses are emitted as requests finish, not in request arrival order. `SendFrame` effects themselves form an ordered sequence per session, which the host must preserve on its transport.

### 6. `Tflush` suppresses replies and waits for terminal work

For `Tflush(new_tag, old_tag)`:

1. If `old_tag` is absent or already terminal, queue `Rflush(new_tag)`.
2. Otherwise mark the old request response-suppressed and emit `CancelFilesystem` or the corresponding policy cancellation for its active operation.
3. Do not queue `Rflush` until the target effect produces a terminal completion/cancellation outcome.
4. Consume the terminal result without emitting the old response.
5. Queue all waiting `Rflush` responses, after which the old tag can be reused.

This ordering guarantees that no old response is emitted after `Rflush`. It does not undo an already committed write or namespace mutation. Backends must return one terminal outcome even when they cannot cancel work. Multiple flushers wait on the same target. Flushing a flush request is represented by the same state rules and must not deadlock or create an uncollectable cycle.

### 7. Capabilities are attached-export promises

The attach policy/filesystem result returns a root QID, root object handle, and `CapabilitySet`. Capabilities use a dependency-free named representation and cover operation availability plus semantic guarantees where the protocol mapping depends on them.

The core decodes every declared `9P2000.L` request. When the current export is known not to support an operation, the core returns `Rlerror(EOPNOTSUPP)` without emitting backend work. It never silently implements a weaker rename, sync, lock, xattr, or atomicity guarantee.

### 8. Error domains remain separate

- `DecodeError`: invalid or incomplete wire representation.
- `SessionError`: invalid state or resource exhaustion attributable to protocol/session handling.
- `FilesystemError`: backend semantic failure with a stable project-defined `LinuxErrno`.
- `CompletionError`: host supplied an unknown, duplicate, or wrong-kind completion.
- `CloseReason`: terminal framing, policy, resource, or host closure reason.

If a valid request has a usable tag, semantic/session failures become `Rlerror`. A malformed frame whose tag cannot be trusted closes the session. Backend-private error text is not exposed on the wire by default.

### 9. The initial platform is `std`, not a runtime

The core uses the Rust standard library for collections and owned memory but performs no operating-system I/O. It contains no async functions, executor integration, socket or file handles, threads, clock reads, randomness, S3 client, WebSocket library, or platform-native errno dependency. `no_std + alloc` is a later compatibility goal.

## Declared Operation Matrix

| Area | Requests whose wire and dispatch behavior is required |
| --- | --- |
| Negotiation/session | `version`, `auth`, `attach`, `flush`, `clunk` |
| Navigation | `walk` |
| Open/create | `lopen`, `lcreate`, `mkdir`, `mknod`, `symlink` |
| Data | `read`, `write`, `readdir`, `fsync` |
| Metadata | `statfs`, `getattr`, `setattr`, `readlink` |
| Namespace | `rename`, `renameat`, `remove`, `unlinkat`, `link` |
| Extended attributes | `xattrwalk`, `xattrcreate` |
| Locking | `lock`, `getlock` |
| Errors | `lerror` replies and project-defined Linux errno mapping |

Base 9P message numbers used by `9P2000.L` are supported where listed. Legacy `9P2000`/`9P2000.u` negotiation and obsolete base `open`, `create`, `stat`, and `wstat` semantics are not implied by this matrix.

The matrix follows the documented `9P2000.L` subset rather than treating every canonical `9P2000` message as mandatory: <https://github.com/chaos/diod/blob/master/protocol.md>.

## Alternatives Considered

### Global `Server` owning every session

This makes cross-session locks easy but imposes a topology, complicates sharding, and conflicts with independently owned session objects. Rejected in favor of an explicit backend coordination contract.

### Async backend trait

An async trait is convenient for an S3 implementation but binds the core to future lifetimes, scheduling behavior, and often an executor ecosystem. Rejected because storage must also be Sans-I/O.

### Synchronous callback trait

A callback is simple but blocks the session and cannot naturally represent arbitrary completion order or cancellation. Rejected for the canonical core; adapters may synchronously drive effects.

### Borrowed or generic buffers from the first release

This could reduce copies but substantially complicates pending-operation lifetimes and public API evolution. Deferred until profiling identifies the required ownership model.

### Implement locks and open-unlink pins only in each session

That would be incorrect across connections. Rejected; these semantics are coordinated by the filesystem implementation using session/open owner identities.

### Promise rollback on `Tflush`

Many filesystem mutations cannot be safely undone after commitment. Rejected; `Tflush` guarantees response suppression and terminal ordering, with cancellation as best effort.

## Security and Correctness Invariants

- No attacker-controlled size or count is allocated before checked bounds validation.
- A tag names at most one active request; an operation ID names exactly one emitted operation.
- A fid is scoped to one session and one attached export.
- A completion cannot affect a request other than the one that emitted its operation ID.
- No old response is emitted after its corresponding `Rflush`.
- A known unsupported capability never falls back to weaker semantics.
- Authorization context reaches the atomic filesystem operation that uses it.
- Closing a session prevents new effects except terminal response, cancellation, and release work.
- The core performs no hidden I/O, including in constructors or destructors.
