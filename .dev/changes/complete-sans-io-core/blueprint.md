# Blueprint: Complete the Sans-I/O Core

## Design Approach

Implement `w9pt` as an externally driven state machine with two Sans-I/O boundaries:

```text
transport bytes/frame                 host completions
         |                                  |
         v                                  v
  checked 9P decoder -> Session -> pending request state
                              |               |
                              v               v
                       SendFrame/Close   Policy/Filesystem effects
```

The primary API shape is conceptual until implementation validates names and ownership:

```rust
let mut session = Session::new(config, context)?;

session.receive_bytes(chunk)?;      // fragmented or coalesced stream input
session.receive_frame(frame)?;      // one owned complete 9P frame
session.complete(completion)?;      // may arrive out of order

while let Some(effect) = session.poll_effect() {
    // The host performs all transport, policy, and filesystem I/O.
}
```

`Session` is independently owned and connection-scoped. The filesystem implementation behind emitted operations coordinates state shared by sessions. The host routes `(SessionId, OperationId)` completions to the correct session.

## Public Contracts

### Effects

- `SendFrame { bytes }`: a complete encoded response; hosts preserve emission order for one session.
- `Filesystem { operation_id, request }`: one owned backend-neutral semantic operation.
- `CancelFilesystem { operation_id }`: best-effort cancellation; it does not promise rollback.
- `Policy { operation_id, request }`: authentication, principal/export mapping, or policy work supplied by the host.
- `CloseSession { reason }`: terminal transport action after the core can no longer continue.

### Completions

- Carry the originating operation ID and an exact typed result.
- May arrive in any order.
- Must reach exactly one terminal result for every emitted policy/filesystem operation, including a cancellation outcome when cancellation is unsupported.
- Unknown, duplicate, or wrong-kind completions are reported as `CompletionError` and do not panic.

### Filesystem boundary

The request/result model covers the declared `9P2000.L` operations using opaque inode/open/xattr/directory handles. Atomic namespace mutation, permission enforcement, stable identity, open-unlinked retention, locking, and durability are obligations of the filesystem implementation, not transport or protocol code.

An attach result supplies the root handle, root QID, and capabilities for that export. The core rejects a known unsupported operation before emitting filesystem work.

## Files to Create or Modify

```text
crates/w9pt/
  Cargo.toml
  src/
    lib.rs
    config.rs
    effect.rs
    error.rs
    limits.rs
    protocol/
      mod.rs
      codec.rs
      message.rs
      types.rs
      flags.rs
    filesystem/
      mod.rs
      request.rs
      response.rs
      types.rs
      capability.rs
      error.rs
    session/
      mod.rs
      decoder.rs
      fid.rs
      pending.rs
      flush.rs
  tests/
    codec_golden.rs
    codec_stream.rs
    session_negotiation.rs
    session_fids.rs
    session_operations.rs
    session_flush.rs
    session_limits.rs
    session_lifecycle.rs
    support/
      mod.rs
      model_driver.rs
  examples/
    drive_session.rs              # only if the doctest is insufficient
```

Exact module splits may be combined where a file would otherwise contain only trivial forwarding code. The public module boundaries and semantic separation matter more than this physical layout.

## Implementation Phases

### Phase 1: Foundations

- Replace the generated template and set crate metadata/lints.
- Define strong identifiers, checked limits, QIDs, flags, attributes, Linux errno, and capability contracts.
- Document invariants and decide which types are public versus internal.

### Phase 2: Wire model and codec

- Implement checked little-endian readers/writers without native-layout casts.
- Define every supported request/reply message.
- Add independent golden vectors, invalid-value tests, and exact consumed-length checks.
- Build one bounded frame decoder used by both stream and complete-frame entry points.

### Phase 3: Effect and filesystem contracts

- Define owned effects/completions and correlation IDs.
- Define host session/policy context and attach result.
- Define backend-neutral operations/results, capabilities, atomicity, cancellation, and durability obligations.
- Add compile-time/API documentation examples for driving a session.

### Phase 4: Session foundation

- Implement version negotiation, negotiated `msize`, tag registration, output ordering, and recoverable versus fatal error behavior.
- Implement attach, walk (including partial walk and new-fid rules), fid tables, clunk, remove, and explicit close/drain.
- Make duplicate/stale state transitions deterministic.

### Phase 5: Operation state machines

- Map open/create/read/write and attribute operations.
- Map directory, namespace, link, symlink, special-file, xattr, sync, and lock operations.
- Enforce capability checks and response-size constraints.
- Support multi-stage operations without exposing protocol details to the filesystem boundary.

### Phase 6: Concurrency and cancellation

- Accept unrelated completions in arbitrary order and reply by tag as each becomes terminal.
- Implement exact `Tflush` response suppression, terminal waiting, cancellation effects, multiple flushers, nested flushes, and late completion handling.
- Enforce limits on pending work and queued bytes under adversarial schedules.

### Phase 7: Verification and documentation

- Add complete operation-matrix, state-machine, lifecycle, and malformed-input coverage.
- Add property/fuzz targets for decoding and action/completion traces.
- Verify the normal dependency graph remains runtime-free and platform-independent.
- Document host ordering, completion, shutdown, capability, security, and compatibility obligations.

## Testing Strategy

- **Golden codec tests:** bytes derived independently from the protocol specification for every message layout.
- **Streaming tests:** every split point, one-byte fragments, multiple frames per chunk, and incomplete tail retention.
- **Malformed input tests:** under-sized/over-sized headers, overflow, invalid UTF-8 policy where applicable, excessive strings/counts, trailing data, and dialect-invalid message types.
- **State tests:** negotiation resets, `NOTAG`, duplicate tags/fids, partial walk, clunk/remove, and close cleanup.
- **Effect tests:** exact request context, opaque handles, capability rejection, response-size clipping, and typed result matching.
- **Concurrency tests:** all relevant completion orders and unrelated reply reordering.
- **Flush tests:** unknown/already-replied target, cancel supported/unsupported, completion races, multiple flushers, nested flushes, committed mutation, and late completion.
- **Limit tests:** input bytes, queued output/effects, tags, fids, path elements, strings, and retained write data.
- **Property/fuzz tests:** decoder never panics or over-allocates; arbitrary valid lifecycle traces preserve table/correlation invariants.
- **Tooling checks:** formatting, linting, documentation, all targets, and dependency-policy inspection.

## Definition of Done

The change is complete when all delta requirements and tasks are implemented, the operation matrix is exhaustive, all tests and documentation checks pass, and no runtime/backend implementation has leaked into `crates/w9pt`.

