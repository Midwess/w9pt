# Analysis: Complete the Sans-I/O Core

## Current State

- The workspace contains one Rust 2024 package at `crates/w9pt` and uses workspace resolver 3.
- `crates/w9pt/src/lib.rs` is still Cargo's generated `add` function plus one unit test.
- The crate has no dependencies, features, package metadata, protocol types, or implementation conventions beyond `.dev/project.md`.
- There are no current specifications under `.dev/specs/`; this change establishes the first normative domain.
- The rename, research, workspace, and scaffold are uncommitted user work. This proposal adds files only under `.dev/changes/` and does not rewrite that work.

## Similar Features and Evidence

The local research identifies two useful Rust Sans-I/O patterns:

- `quinn-proto` separates protocol state mutation from transport execution and exposes work through polling.
- `rustls::unbuffered` lets callers own buffers and drive all I/O.

The relevant 9P design evidence is the stock `9P2000.L` wire contract, Plan 9's tag/fid model, Linux v9fs behavior, and the diod implementer documentation already cited in `.dev/research/9p-s3-filesystem.md`. ZeroFS is evidence that the wider 9P/S3 system is feasible, but its AGPL implementation is not an implementation source for this Apache-2.0 crate.

## Architectural Findings

### Independent sessions and shared semantics

`Session` should remain the primary state object. It owns one connection's negotiated dialect/`msize`, decoder buffer, tags, fids, in-flight operations, and queued effects. A host-supplied `SessionId` makes operation and lock-owner identity globally routable without a singleton.

Cross-session atomicity, stable inode identity, open-inode pins, permissions, namespace transactions, and lock coordination belong to the backend-neutral filesystem contract. The core does not hide these behind `Arc`, a global server, or a background task.

### Owned effects and completions

Public effect payloads use owned values, including `Vec<u8>` for frames and data. This avoids borrowing across arbitrary host scheduling and out-of-order completion. `receive_frame(Vec<u8>)` can take ownership of a message-transport frame; `receive_bytes(&[u8])` copies only what the bounded stream decoder must retain.

### Semantic request granularity

The core emits high-level filesystem operations such as walk, open, positioned read/write, rename, link, unlink, lock, and sync. Each mutation carries identity/export context and is authorized and committed atomically by the implementation behind the contract. Low-level S3 keys, extents, manifests, transactions, and retries are deliberately absent.

### Correlation and completion

Pending requests are indexed by both 9P tag and opaque `OperationId`. Operation IDs are never reused within a session. Filesystem and policy completions may arrive in any order; unknown, duplicate, or result-type-mismatched completions are host API errors and cannot panic or mutate unrelated state.

### Cancellation

`Tflush` cancels the protocol reply, not the underlying transaction. A pending target is marked response-suppressed, its active work receives a cancellation effect, and `Rflush` waits until that work reaches a terminal completion. This ensures no response for the old tag follows `Rflush`, while explicitly allowing a mutation to have already committed.

### Capabilities and errors

An attach completion returns a root handle/QID plus the export's `CapabilitySet`. Known unsupported operations return `EOPNOTSUPP` without backend work. The contract uses project-defined, target-independent Linux errno values and separates malformed wire errors, session state errors, filesystem semantic errors, and host completion misuse.

### Dependency and portability baseline

The initial core targets `std` and uses no normal dependencies, unsafe wire casts, async functions, sockets, filesystem calls, background threads, clock access, or random-number source. `no_std + alloc` and borrowed zero-copy APIs remain future design work rather than acceptance criteria for this change.

## Affected Files

| Path | Change |
| --- | --- |
| `crates/w9pt/Cargo.toml` | Add package metadata, features only if justified, and development tooling; retain a runtime-free normal dependency set. |
| `crates/w9pt/src/lib.rs` | Replace the template and expose the documented public modules/API. |
| `crates/w9pt/src/config.rs` | Session configuration, host context, and validated limits. |
| `crates/w9pt/src/effect.rs` | Owned effects, completions, identifiers, and completion contracts. |
| `crates/w9pt/src/error.rs` | Decode, session, completion, and close/error domains. |
| `crates/w9pt/src/limits.rs` | Hard and negotiated resource limits with checked accounting. |
| `crates/w9pt/src/protocol/*` | Wire scalars, flags, messages, checked codec, and frame decoder. |
| `crates/w9pt/src/filesystem/*` | Backend-neutral requests/results, capabilities, handles, attributes, and semantic errors. |
| `crates/w9pt/src/session/*` | Negotiation, fid/tag/pending tables, dispatch, cancellation, and shutdown. |
| `crates/w9pt/tests/*` | Golden vectors, framing, operation mapping, state-machine traces, races, limits, and lifecycle coverage. |
| `crates/w9pt/examples/*` | Minimal deterministic host-driving example if it improves crate documentation. |

No S3, WebSocket, Tokio, FUSE, or daemon crate is affected.

## Conventions to Follow

- Protocol modules know 9P but never S3; filesystem modules know semantic operations but never 9P encoding.
- All length/count arithmetic is checked before allocation or slicing.
- Wire integers are decoded explicitly as little-endian values; no native-layout casts.
- Public identifiers are strong newtypes rather than interchangeable integers.
- Public state transitions return typed errors; malformed or adversarial input must not panic.
- Effects are polled and completed explicitly. `Drop` performs no hidden I/O or required cleanup.
- Emitted `SendFrame` effects are ordered per session, although different request tags may complete in any order.
- Capability values are enforceable promises, not optimization hints.
- Tests use independent expected bytes and model traces, not only self-round-trips.
- No implementation may be copied or translated from AGPL ZeroFS sources.

## Risks and Dependencies

| Risk | Mitigation |
| --- | --- |
| Incorrect operation coverage | Maintain one exhaustive message/dispatch test table for the declared dialect. |
| Allocation denial of service | Validate outer size first and account every retained byte against configured limits. |
| Tag/fid state corruption | Centralize insertion, replacement, partial-walk, clunk, and terminal-transition invariants. |
| Flush/completion races | Use explicit pending states and table-driven interleaving tests. |
| TOCTOU authorization | Carry request context into atomic semantic operations rather than pre-authorizing mutations in protocol code. |
| Backend overpromises | Define capability, atomicity, durability, cancellation, and idempotency obligations for every request class. |
| Cleanup leaks | Provide explicit close/drain state and document the consequence of abandoning it. |
| API premature optimization | Start with owned values and `std`; benchmark before introducing borrowed/generic buffer complexity. |

