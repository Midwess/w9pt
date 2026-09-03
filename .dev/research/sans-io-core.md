# Sans-I/O core and Rust filesystem interoperability

Research status: initial architectural survey  
Last checked: 2026-09-01

## Conclusion

The first `w9pt` crate should be a Sans-I/O state machine and the canonical public API of the project.

It should perform no socket, WebSocket, file, S3, clock, thread, or async-runtime I/O. Instead, the embedding application supplies input events and backend completions; the core advances protocol/filesystem state and emits typed effects that the application executes.

This design can interoperate well with Rust's standard **I/O traits** but cannot transparently replace Rust's concrete **filesystem module**:

- `std::io::Read`, `Write`, and `Seek` are public traits and can be implemented by w9pt adapter types.
- `std::fs::File`, `Metadata`, `DirEntry`, `ReadDir`, and functions such as `std::fs::read` are concrete host-OS filesystem APIs, not pluggable provider traits.
- Code using `std::fs` can access w9pt only when a separate adapter mounts w9pt into the operating system namespace.

## 1. Two Sans-I/O boundaries

There are two kinds of external I/O to remove from the core.

### Network/transport I/O

Input:

- arbitrary TCP/Unix byte chunks;
- or one complete binary WebSocket/virtio message;
- connection opened/closed;
- authenticated identity and policy context;
- cancellation and resource-limit decisions.

Output:

- encoded 9P response frames;
- close-session events;
- transport backpressure/capacity information;
- optional deadlines or keepalive requests if private extensions introduce time.

### Storage/backend I/O

Input:

- success/failure completions for lookup, metadata, directory, data, lock, and durability operations;
- caller-provided clock/randomness results when required;
- writer-fencing or transaction outcomes.

Output:

- typed backend requests;
- cancellation of outstanding requests;
- flush/commit requests;
- cache invalidation or lifecycle effects.

If the core calls an async `Backend` trait directly, it is network-Sans-I/O but not fully Sans-I/O. A fully portable core should model backend work as effects and completions too.

## 2. Proposed core shape

The API should resemble an externally driven protocol engine such as `quinn-proto`: handlers accept inputs, mutators accept commands/completions, and polling methods return actions without performing system I/O.

Conceptual API—not yet a committed Rust signature:

```rust
let mut session = Session::new(config, identity);

session.receive_bytes(network_input)?;

while let Some(effect) = session.poll_effect() {
    match effect {
        Effect::Send { bytes } => transport.send(bytes),
        Effect::Storage { id, request } => executor.spawn(async move {
            let result = storage.execute(request).await;
            completions.send(Completion::Storage { id, result });
        }),
        Effect::CancelStorage { id } => storage.cancel(id),
        Effect::Close { reason } => transport.close(reason),
    }
}

session.complete(completion)?;
```

The actual core API should not contain the `async` block, channel, or transport calls shown in the host loop.

### Suggested input events

```text
Input::Bytes(bytes)
Input::Frame(complete_9p_message)
Input::StorageCompleted { operation_id, result }
Input::StorageCancelled { operation_id }
Input::TransportClosed
Input::Now(timestamp)                  optional/private dialects
Input::Random(bytes)                   IDs/nonces supplied by caller
```

### Suggested emitted effects

```text
Effect::SendFrame(bytes)
Effect::Storage { operation_id, request }
Effect::CancelStorage { operation_id }
Effect::NeedInputCapacity { bytes }
Effect::CloseSession(reason)
Effect::WakeAt(timestamp)              optional/private dialects
```

### State owned by the core

- negotiated version and `msize`;
- input framing buffer and bounded response queue;
- request tags and outstanding protocol operations;
- fid table, attach roots, credentials, open modes, and per-session resources;
- mapping from backend operation IDs to 9P tags and cancellation state;
- retry/idempotency state defined by a future private dialect;
- protocol-visible capability and error mapping.

The core must accept backend completions out of order while preserving 9P tag correlation and operation ordering guarantees.

## 3. Codec boundary

The core should support both stream and message transports:

- `receive_bytes` incrementally reads the four-byte little-endian 9P length, validates it, and waits for the complete message.
- `receive_frame` handles WebSocket/virtio input where one transport message is one complete 9P message.

Both paths must share one decoder and enforce:

- minimum seven-byte header;
- declared length equals complete message size;
- hard configured maximum and negotiated `msize`;
- bounded buffering before allocation;
- known/allowed message type for the negotiated dialect;
- tag/fid and in-flight resource limits.

The core should emit complete encoded 9P frames. TCP adapters write them as bytes; WebSocket adapters send one binary message per frame.

## 4. Backend request model

The semantic layer should emit backend-neutral requests, not S3 calls:

```text
Lookup(parent_inode, name)
GetAttr(inode, mask)
Open(inode, access, identity)
Read(inode, offset, length, open_token)
Write(inode, offset, bytes, open_token)
ReadDir(inode, cursor, limit)
Create(parent, name, kind, attrs)
SetAttr(inode, changes)
Rename(old_parent, old_name, new_parent, new_name)
Link(parent, name, target_inode)
Unlink(parent, name, flags)
Lock(inode, owner, range, mode)
Flush(scope, durability)
```

Operation contracts must state atomicity, idempotence, authorization point, durability, cancellation, and retry behavior before signatures are stabilized.

S3-specific extent lookup, range GET, multipart upload, conditional PUT, segment publication, and garbage collection belong in the S3 adapter/engine, not the 9P core.

## 5. Rust standard-library compatibility

### What works: `std::io`

Rust's standard byte-stream abstractions are traits:

- [`Read`](https://doc.rust-lang.org/std/io/trait.Read.html)
- [`Write`](https://doc.rust-lang.org/std/io/trait.Write.html)
- [`Seek`](https://doc.rust-lang.org/std/io/trait.Seek.html)

A blocking adapter can expose a w9pt open handle:

```rust
struct W9ptFile<D> {
    driver: D,
    handle: OpenHandle,
    cursor: u64,
}

impl<D: BlockingDriver> std::io::Read for W9ptFile<D> { /* drive effects */ }
impl<D: BlockingDriver> std::io::Write for W9ptFile<D> { /* drive effects */ }
impl<D: BlockingDriver> std::io::Seek for W9ptFile<D> { /* update cursor */ }
```

Then generic libraries work:

```rust
fn parse<R: std::io::Read + std::io::Seek>(input: R) { /* ... */ }

parse(w9pt_file);
```

The adapter—not the core—blocks while driving storage effects to completion. `Write::flush` should flush adapter buffers, while an explicit `sync_all`/durability API is still needed because `std::io::Write::flush` is not defined as filesystem `fsync`.

Separate optional crates can provide:

- Tokio `AsyncRead`, `AsyncWrite`, and `AsyncSeek`;
- futures-io traits;
- byte-stream and buffered-reader helpers;
- positioned `read_at`/`write_at` APIs that avoid a shared cursor.

### What does not work directly: `std::fs`

The standard [`std::fs`](https://doc.rust-lang.org/std/fs/index.html) module explicitly manipulates the local filesystem. Its types have private/internal representations:

- [`std::fs::File`](https://doc.rust-lang.org/std/fs/struct.File.html) is a concrete OS file object;
- `Metadata`, `DirEntry`, and [`ReadDir`](https://doc.rust-lang.org/std/fs/struct.ReadDir.html) are concrete standard-library types;
- `std::fs::read`, `rename`, `metadata`, and related functions dispatch to the operating system and expose no provider hook.

Therefore w9pt cannot make this call target an in-process backend:

```rust
std::fs::read("w9pt:/some/file") // no custom scheme/provider dispatch
```

Nor should it fabricate an `std::fs::File` or raw file descriptor for an object that the kernel does not own.

There are three valid interoperability levels:

1. **w9pt-native filesystem API**
   - `Filesystem::open`, `metadata`, `read_dir`, `rename`, and other project-owned types.
   - Best in-process fidelity and no forced blocking/runtime dependency.

2. **standard I/O trait adapters**
   - Opened w9pt handles implement `Read`/`Write`/`Seek` through a blocking driver.
   - Generic stream consumers work, but path/directory APIs remain w9pt-specific.

3. **OS mount adapter**
   - FUSE, native 9P, or another kernel-facing adapter mounts w9pt at a real path.
   - Unmodified `std::fs`, command-line tools, and other processes work through the OS.
   - Requires runtime integration outside the Sans-I/O core.

Accepting `std::path::Path`/`PathBuf` is possible for ergonomic path input, but those types only represent host-platform path syntax; they do not provide filesystem dispatch.

## 6. Suggested workspace evolution

Keep the first crate as the stable Sans-I/O core:

```text
crates/
  w9pt/                 Sans-I/O codec, sessions, semantics, effects

future optional crates:
  w9pt-s3/              S3 storage effect executor/backend
  w9pt-std-io/          blocking Read/Write/Seek adapters
  w9pt-tokio/           Tokio transport and async file adapters
  w9pt-websocket/       WebSocket framing adapter(s)
  w9pt-fuse/            OS mount adapter
  w9pt-test/            backend/protocol conformance harness
```

Runtime crates may depend on the core; the core must not depend on them.

## 7. Testing consequences

Sans-I/O enables deterministic tests without sockets, S3, sleeps, or executors:

- feed fragmented headers/bodies one byte at a time;
- feed multiple messages in one chunk;
- reorder backend completions;
- fail/cancel every backend operation;
- test `Tflush` races deterministically;
- cap buffers/fids/tags and exercise exhaustion;
- snapshot state-machine traces;
- fuzz decoder inputs and action/completion sequences;
- use a model backend to verify filesystem semantics;
- reuse the same traces against sync, Tokio, WebSocket, and S3 adapters.

This follows established Rust Sans-I/O patterns. [`quinn-proto`](https://docs.rs/quinn-proto/latest/quinn_proto/struct.Connection.html) separates event handlers and state mutations from `poll_*` outputs, while [`rustls::unbuffered`](https://docs.rs/rustls/latest/rustls/unbuffered/) requires callers to own buffers and perform all I/O.

## 8. Decisions and open questions

Accepted direction:

- `crates/w9pt` is the first and central Sans-I/O crate.
- It has no network/storage runtime dependencies.
- Storage work is represented as typed effects and completions.
- WebSocket is an adapter, not a core dependency.
- Standard `std::io` traits are supported through optional handle adapters.
- Direct `std::fs` substitution is impossible without an OS mount.

Still to specify before replacing Cargo's generated template:

- exact effect ownership and buffer model;
- how sessions share filesystem/global state safely;
- operation IDs and out-of-order completion rules;
- cancellation semantics for `Tflush`;
- synchronous versus async backend completion lifetimes;
- `no_std + alloc` target or standard-library baseline;
- error and capability model;
- blocking adapter behavior and durability mapping;
- whether backend transaction coordination belongs in the core or S3 engine.

## 9. References

- [Rust `std::fs`](https://doc.rust-lang.org/std/fs/index.html)
- [Rust `std::io::Read`](https://doc.rust-lang.org/std/io/trait.Read.html)
- [Rust `std::io::Write`](https://doc.rust-lang.org/std/io/trait.Write.html)
- [Rust `std::io::Seek`](https://doc.rust-lang.org/std/io/trait.Seek.html)
- [Quinn protocol state machine](https://docs.rs/quinn-proto/latest/quinn_proto/struct.Connection.html)
- [rustls unbuffered/Sans-I/O API](https://docs.rs/rustls/latest/rustls/unbuffered/)
