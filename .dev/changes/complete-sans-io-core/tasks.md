# Tasks: Complete the Sans-I/O Core

## Progress: [38/38]

### 1. Crate foundations

- [x] 1.1 Replace Cargo's generated `add` template with documented module exports and a minimal crate-level Sans-I/O example.
- [x] 1.2 Add package metadata, documentation/lint policy, and dependency-policy checks to `crates/w9pt/Cargo.toml`.
- [x] 1.3 Define strong public identifiers for sessions, operations, tags, fids, QIDs, and opaque filesystem/open handles.
- [x] 1.4 Implement validated configuration and checked limits for frames, queues, tags, fids, walks, strings, and retained write data.

### 2. Protocol model

- [x] 2.1 Define target-independent `9P2000.L` scalar types, constants, message numbers, flags, masks, and project-owned Linux errno values.
- [x] 2.2 Define request and response types for the complete declared operation matrix.
- [x] 2.3 Define QID, statfs, attributes, directory entries, lock, xattr, and identity wire structures with validation rules.
- [x] 2.4 Add an exhaustive table test proving every declared message type has an intentional decode, encode, and dispatch classification.

### 3. Checked codec and framing

- [x] 3.1 Implement checked little-endian readers and writers without unsafe or native-layout casts.
- [x] 3.2 Implement request decoding with exact length consumption, bounded strings/lists/data, and typed decode errors.
- [x] 3.3 Implement response encoding with checked size arithmetic and negotiated `msize` enforcement.
- [x] 3.4 Implement one bounded incremental frame decoder for fragmented/coalesced stream input and complete-frame validation.
- [x] 3.5 Add independent golden vectors plus malformed, truncated, overflow, trailing-data, and every-split-point tests.

### 4. Effects and filesystem contract

- [x] 4.1 Define owned `Effect` and `Completion` APIs for frames, filesystem work, policy work, cancellation, and closure.
- [x] 4.2 Define exact operation-ID allocation, completion-kind matching, terminal-completion, and host routing rules.
- [x] 4.3 Define backend-neutral filesystem requests/results for every operation in the matrix using opaque handles and request context.
- [x] 4.4 Define attached-export capabilities and enforceable atomicity, authorization, cancellation, open-unlink, lock, and durability obligations.
- [x] 4.5 Define separate decode, session, completion, filesystem, and close error domains with stable `Rlerror` mapping.

### 5. Session foundation

- [x] 5.1 Implement `Session` construction, host context, effect polling, byte/frame ingestion, completion ingestion, and quiescence reporting.
- [x] 5.2 Implement exact `9P2000.L` version negotiation, `NOTAG`, negotiated `msize`, and renegotiation/reset behavior.
- [x] 5.3 Implement tag registration, duplicate-tag rejection, pending tables indexed by tag and operation ID, and response ordering.
- [x] 5.4 Implement host-driven auth/attach policy flow and install the returned export root, QID, context, and capabilities.
- [x] 5.5 Implement fid allocation, clone/partial walk semantics, replacement rules, open state, clunk, remove, and stale-fid errors.
- [x] 5.6 Implement explicit begin-close/drain behavior that emits and tracks cancellation/release work without hidden `Drop` I/O.

### 6. Operation state machines

- [x] 6.1 Implement open/create/read/write dispatch, response sizing, offset/count checks, and opaque open-handle lifecycle.
- [x] 6.2 Implement statfs/getattr/setattr/readlink and their mask, attribute, and error mappings.
- [x] 6.3 Implement readdir, mkdir, mknod, symlink, link, rename, renameat, remove, and unlinkat mappings.
- [x] 6.4 Implement xattrwalk/xattrcreate and their fid/size/lifecycle rules.
- [x] 6.5 Implement fsync, lock, and getlock mappings with capability and semantic guarantee checks.

### 7. Concurrency, cancellation, and limits

- [x] 7.1 Accept unrelated filesystem/policy completions in every order and reject unknown, duplicate, stale, or wrong-kind completions safely.
- [x] 7.2 Implement `Tflush` response suppression, terminal waiting, best-effort cancellation effects, and non-rollback semantics.
- [x] 7.3 Cover unknown/already-replied targets, cancel-supported/unsupported outcomes, completion races, multiple flushers, nested flushes, and late results.
- [x] 7.4 Enforce all configured limits and queue/backpressure rules under adversarial input and delayed completions.

### 8. Verification and documentation

- [x] 8.1 Build a deterministic model driver and operation-mapping tests for every request, result, capability rejection, and error path.
- [x] 8.2 Add session tests for negotiation/reset, duplicate tags/fids, partial walks, out-of-order replies, and close/drain lifecycle.
- [x] 8.3 Add property/fuzz coverage for decoder inputs and state-machine action/completion traces without relying solely on round trips.
- [x] 8.4 Document the host driving loop, effect ownership, output ordering, completion obligations, shutdown, capabilities, and intentional exclusions.
- [x] 8.5 Run formatting, lint, documentation, all-target tests, and dependency inspection; record that the core contains no runtime/backend I/O dependencies.

## Verification Notes

- 2026-09-02: `cargo fmt --all -- --check` passed.
- 2026-09-02: `cargo clippy --workspace --all-targets -- -D warnings` passed.
- 2026-09-02: `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps` passed.
- 2026-09-02: `cargo test --workspace --all-targets` passed (45 tests).
- 2026-09-02: `cargo test --workspace --doc` passed (1 doctest).
- 2026-09-02: `cargo tree --workspace -e normal` contains only `w9pt`; the core has no normal dependencies and therefore no runtime, transport, native errno, filesystem adapter, object-store SDK, or backend I/O dependency.
- The isolated `crates/w9pt/fuzz` harness is not a workspace/runtime dependency and uses `libfuzzer-sys` only for explicit fuzz runs.
