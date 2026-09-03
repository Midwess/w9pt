# Delta for Sans-I/O Core

## ADDED Requirements

### Requirement: Fully Sans-I/O Execution
The core SHALL perform no transport, filesystem, object-store, clock, randomness, thread, executor, or background-task I/O and SHALL expose all externally executed work as typed effects and completions.

#### Scenario: Drive a session without a runtime
- GIVEN a configured session and caller-supplied context
- WHEN the caller supplies protocol input and polls the session
- THEN the core returns zero or more owned effects without blocking on external work
- AND the caller can advance the session by supplying typed completions

#### Scenario: External value is required
- WHEN protocol processing requires a policy decision or another external value
- THEN the core emits a typed request for that value or accepts it through an explicit input
- AND the core does not obtain the value from the operating system itself

### Requirement: Independent Session Ownership
The core SHALL represent each transport connection as an independently owned session with no mandatory process-global server, async runtime, thread, or shared mutex.

#### Scenario: Host drives multiple sessions
- GIVEN two sessions with distinct host-supplied session identifiers
- WHEN both sessions emit filesystem operations
- THEN their operation identifiers are unambiguously routable by session and operation
- AND the host may schedule their effects independently

#### Scenario: Shared filesystem semantics are required
- WHEN separate sessions access the same export
- THEN cross-session namespace, open-inode, and lock coordination is expressed as a filesystem-contract obligation
- AND the core does not install hidden global state

### Requirement: Stream and Complete-Frame Input
The core SHALL accept arbitrary stream byte chunks and owned complete 9P frames through one shared checked message decoder.

#### Scenario: Fragmented and coalesced stream input
- WHEN the caller supplies a header or body across arbitrary chunk boundaries or supplies multiple frames in one chunk
- THEN the decoder retains only the bounded incomplete tail
- AND dispatches each complete frame exactly once

#### Scenario: Complete frame has an inconsistent length
- WHEN a complete-frame input's declared little-endian size differs from its supplied byte length
- THEN the core rejects the frame as malformed
- AND does not dispatch any part of it

### Requirement: Bounded Wire Decoding
The core SHALL validate all wire sizes, counts, offsets, strings, and arithmetic before allocation, slicing, or state mutation.

#### Scenario: Frame exceeds a configured or negotiated limit
- WHEN a declared frame size exceeds the hard configured maximum or negotiated `msize`
- THEN the core rejects the frame without allocating the declared size
- AND follows the documented recoverable-error or close-session rule

#### Scenario: Nested count exceeds remaining bytes
- WHEN a string, list, walk element, directory entry, or data count exceeds the containing frame
- THEN decoding fails with a typed error
- AND the core does not panic or retain partial session mutations

### Requirement: Exact 9P2000.L Negotiation
The core SHALL negotiate stock `9P2000.L`, enforce `NOTAG` and `msize` rules, and reset connection-scoped protocol state when version negotiation requires it.

#### Scenario: Supported version negotiation
- WHEN the client sends a valid `Tversion` for `9P2000.L` within configured limits
- THEN the core replies with the supported version and effective `msize`
- AND subsequent messages are validated against that negotiated state

#### Scenario: Unsupported dialect
- WHEN the client requests a dialect the core does not implement
- THEN the core responds using the standard unknown-version behavior
- AND does not silently negotiate legacy or private semantics

#### Scenario: Renegotiation resets state
- GIVEN the session has fids or in-flight requests
- WHEN a valid new `Tversion` begins renegotiation
- THEN the core invalidates prior connection-scoped state according to the protocol lifecycle
- AND emits any required cancellation or release effects explicitly

### Requirement: Host-Supplied Authentication and Policy
The core SHALL represent `Tauth`, authentication-fid exchange, principal mapping, attach/export selection, and authorization decisions without embedding an authentication mechanism or secret source.

#### Scenario: Authentication is not required
- WHEN the host policy rejects `Tauth` because the connection already has an authenticated identity or the export requires no protocol auth exchange
- THEN the core returns the policy-selected `Rlerror`
- AND attach may proceed only under the host's explicit policy result

#### Scenario: Authentication fid is used
- GIVEN host policy accepted `Tauth` and installed an authentication fid
- WHEN the client reads, writes, or clunks that authentication fid
- THEN the core routes the exchange through typed policy effects
- AND does not treat authentication bytes as ordinary filesystem file data

#### Scenario: Attach is authorized
- WHEN the host completes attach policy with a principal, export, root, and capabilities
- THEN the core binds the new root fid and all derived fids to that context
- AND later requests carry the bound context to filesystem operations

### Requirement: Declared Operation Matrix
The core SHALL decode, encode, and intentionally dispatch or reject the stock `9P2000.L` requests `version`, `auth`, `attach`, `flush`, `walk`, `clunk`, `lopen`, `lcreate`, `mkdir`, `mknod`, `symlink`, `read`, `write`, `readdir`, `fsync`, `statfs`, `getattr`, `setattr`, `readlink`, `rename`, `renameat`, `remove`, `unlinkat`, `link`, `xattrwalk`, `xattrcreate`, `lock`, and `getlock`.

#### Scenario: Supported export operation
- GIVEN an attached export advertises the required capability
- WHEN the client sends a valid request in the declared operation matrix
- THEN the core emits the corresponding backend-neutral work or completes it from protocol state
- AND encodes the matching response when the work becomes terminal

#### Scenario: Unsupported export operation
- GIVEN an attached export does not advertise the required capability
- WHEN the client sends an otherwise valid request in the declared operation matrix
- THEN the core emits `Rlerror` with the documented unsupported-operation errno
- AND does not silently emulate weaker semantics

### Requirement: Tag Correlation and Concurrent Completion
The core SHALL correlate every active request by its 9P tag and every external operation by an opaque operation identifier that is not reused within the session.

#### Scenario: Completions arrive out of order
- GIVEN multiple requests with distinct tags have active operations
- WHEN their typed completions arrive in an order different from request arrival
- THEN each result advances only its originating request
- AND replies may be emitted by completion order while retaining their original tags

#### Scenario: Duplicate active tag
- GIVEN a request tag is still active
- WHEN another request uses the same request tag
- THEN the core rejects the duplicate deterministically
- AND the original pending request remains unchanged

#### Scenario: Invalid host completion
- WHEN the host supplies an unknown, duplicate, stale, or wrong-kind completion
- THEN the core returns a typed completion error
- AND does not panic or mutate an unrelated request

### Requirement: Fid Lifecycle
The core SHALL maintain session-scoped fid state for attach, walk, open, xattr, clunk, remove, and shutdown, including partial-walk and replacement rules.

#### Scenario: Partial walk succeeds
- GIVEN a valid source fid and a multi-element walk
- WHEN the filesystem resolves only a non-zero prefix according to 9P walk semantics
- THEN the core returns QIDs for the resolved prefix
- AND installs the destination fid in the state required by the protocol

#### Scenario: Fid is clunked
- GIVEN a fid owns an opaque open or xattr handle
- WHEN the client sends `Tclunk`
- THEN the core emits any required release operation and retires the fid only through its documented terminal transition
- AND later use of the retired fid returns the documented error

#### Scenario: Session closes
- WHEN the host begins explicit session shutdown or the transport closes
- THEN the core prevents new ordinary work and emits cancellation/release effects for live state
- AND reports when shutdown is drained without relying on `Drop` to perform I/O

### Requirement: Backend-Neutral Filesystem Contract
The core SHALL express filesystem work as high-level backend-neutral operations using opaque object/open handles and request context, without exposing 9P wire encoding or S3 storage mechanics.

#### Scenario: Atomic namespace mutation
- WHEN the core emits a rename, link, unlink, create, or attribute mutation
- THEN the operation carries the identity and operands needed for atomic authorization and mutation
- AND the contract does not split the mutation into raceable protocol-side checks

#### Scenario: Positioned data operation
- WHEN the client issues a read or write with an explicit offset and count
- THEN the emitted operation preserves the positioned-I/O semantics and opaque open handle
- AND contains no S3 key, extent, segment, SDK, or transaction representation

### Requirement: Attached Export Capabilities
The core SHALL associate each attached export with an enforceable capability set covering supported operations and required semantic guarantees.

#### Scenario: Attach completes
- WHEN policy/filesystem attach work succeeds
- THEN the result supplies the export root handle, root QID, context, and capability set
- AND every fid derived from that root remains bound to the export

#### Scenario: Backend cannot promise a guarantee
- WHEN an operation requires atomicity, durability, locking, xattr, or open-unlink behavior that the export cannot provide
- THEN the export omits that capability and the core rejects the operation explicitly
- AND the core does not advertise or fabricate the stronger guarantee

### Requirement: Stable Error Domains
The core SHALL separate wire decoding, session state, host completion misuse, filesystem semantic failures, and terminal close reasons, and SHALL map valid request failures to project-defined `9P2000.L` Linux errno values.

#### Scenario: Filesystem operation fails
- GIVEN a valid request and tag emitted a filesystem operation
- WHEN the backend returns a typed semantic failure
- THEN the core encodes `Rlerror` with the stable mapped errno for the original tag
- AND does not expose backend-private implementation details on the wire by default

#### Scenario: Tag cannot be trusted
- WHEN a malformed frame cannot be safely associated with a valid request tag
- THEN the core emits a close-session effect with a typed reason
- AND does not fabricate a protocol reply for an untrusted tag

### Requirement: Tflush Reply Cancellation
The core SHALL implement `Tflush` as cancellation and suppression of the target request's reply, without promising rollback of already performed filesystem work.

#### Scenario: Target request is active
- GIVEN `oldtag` identifies a request with active external work
- WHEN the client sends `Tflush` for `oldtag`
- THEN the core marks the old response suppressed and emits a best-effort cancellation effect
- AND does not emit `Rflush` until the target work reports a terminal outcome

#### Scenario: Mutation already committed
- GIVEN the flushed operation committed before cancellation was observed
- WHEN its terminal completion arrives
- THEN the core consumes the result without replying on the old tag
- AND does not claim that `Tflush` rolled back the mutation

#### Scenario: Target is absent or already replied
- WHEN `Tflush` names a tag with no active request
- THEN the core emits `Rflush`
- AND does not disturb another request or reused operation identifier

#### Scenario: Multiple or nested flushes
- GIVEN one or more flush requests target active requests, including another flush request
- WHEN terminal outcomes arrive in any valid order
- THEN every flush request reaches one deterministic terminal response or suppression state
- AND no old response is emitted after its corresponding `Rflush`

### Requirement: Bounded Session State
The core SHALL enforce configurable limits for retained input, queued frames/effects, in-flight tags, fids, walk elements, strings, and pending write bytes.

#### Scenario: Resource limit is exhausted
- WHEN accepting a valid request would exceed a configured session limit
- THEN the core rejects or backpressures it using the documented behavior
- AND retained memory remains within the configured accounting bound

#### Scenario: Delayed completions fill queues
- GIVEN the host delays filesystem completions or transport sends
- WHEN pending or output accounting reaches its limit
- THEN the core stops accepting the corresponding additional work or returns an explicit error
- AND does not grow an unbounded queue

### Requirement: Ordered Transport Effects
The core SHALL emit complete response frames in a per-session sequence that the host can preserve while allowing unrelated request tags to complete out of arrival order.

#### Scenario: Host preserves emission order
- GIVEN two `SendFrame` effects are polled from one session
- WHEN the host writes them in the order emitted
- THEN the peer observes a valid response sequence
- AND any `Tflush` ordering guarantee remains intact

### Requirement: Deterministic Conformance Testing
The core SHALL provide deterministic tests and reusable test support that validate wire compatibility, operation mapping, lifecycle invariants, limits, concurrency, and cancellation without sockets or real storage.

#### Scenario: Codec implementation has a symmetric bug
- WHEN the encoder and decoder agree with each other but disagree with an independent protocol vector
- THEN a golden-vector test fails
- AND the failure does not depend solely on an encode/decode round trip

#### Scenario: Completion race is replayed
- WHEN a model driver supplies a recorded sequence of inputs, effects, cancellations, and out-of-order completions
- THEN the session produces the same state/effect trace deterministically
- AND invariant violations are observable without timing or sleeps

### Requirement: Runtime-Free Rust Core
The core SHALL use Rust 2024 with a `std` baseline while keeping its normal dependency graph free of async runtimes, sockets, WebSocket clients, S3 SDKs, platform filesystem engines, and native errno dependencies.

#### Scenario: Core dependencies are inspected
- WHEN the normal dependency graph and public API are reviewed
- THEN no Tokio, async executor, network transport, object-store SDK, filesystem adapter, or `libc`-derived errno surface is present
- AND development-only testing tools do not become runtime requirements
