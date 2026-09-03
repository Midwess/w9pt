# 9P over an S3-backed filesystem

Research status: initial feasibility survey  
Last checked: 2026-09-01  
Related research: [How NFS works](./nfs.md), [SteamPipe's chunk-based content system](./steampipe-content-system.md)

## Executive conclusion

It is feasible to expose an S3-backed filesystem through 9P, including ordinary mutable files, random reads/writes, directories, atomic rename, hard links, symlinks, permissions, truncation, `fsync`, and advisory byte-range locks. However, those semantics do **not** come from 9P and S3 alone:

- 9P is the client/server filesystem protocol.
- S3 is the durable object/blob substrate.
- A real filesystem engine between them must implement inodes, directories, extent maps, metadata transactions, open-file state, locking, caching, crash recovery, and garbage collection.

A direct translation such as `9P path /a/b -> S3 key a/b` cannot provide a full filesystem. It is suitable for read-mostly or create-and-replace object workloads, but fails on atomic directory rename, random overwrite, stable inode identity, hard links, open-unlink behavior, mutable POSIX metadata, cross-object transactions, and locking.

The strongest implementation evidence is [ZeroFS](https://github.com/Barre/ZeroFS), an active AGPL-3.0/commercial Rust project that already exposes S3-compatible storage through 9P2000.L, NFS, and NBD. It stores 32 KiB logical extents inside immutable object-store segments and keeps inode/directory/extent metadata in an LSM tree stored on the same object store. It uses local memory/disk caches, conditional object creation for fencing, metadata manifests for durability, and server-side lock state. w9pt can study these backend techniques while deliberately choosing a different library-first process and API model.

The practical recommendation is:

1. Keep 9P framing/session types, filesystem semantics, and storage backend interfaces as separate embeddable library layers.
2. Treat the S3 bucket prefix as an **opaque private filesystem format**, not as a browsable one-object-per-file layout.
3. Use 9P2000.L as the initial protocol baseline, driven by application-owned transports.
4. Let the embedding application supply lifecycle, authentication, policy, observability, and runtime integration.
5. Store file data as fixed-size logical extents packed into much larger immutable S3 segment objects.
6. Publish data before metadata and make a metadata manifest/root pointer the atomic durability boundary.
7. Add multi-writer or high-availability primitives only through explicit backend/session interfaces rather than a mandatory built-in daemon topology.

“Full filesystem” must still be scoped. Stock 9P2000.L can expose a broad Linux/POSIX subset, but it does not cover every Linux filesystem feature such as all `renameat2` modes, every `fallocate` mode, reflinks, arbitrary `ioctl`, leases/delegations, reliable change notification, or transparent session recovery. A capability matrix is required.

## 1. What 9P contributes

9P is Plan 9's stateful remote filesystem protocol. A client does not send an absolute path with every operation. Instead, it attaches to a server tree, walks path components, and receives per-session **file identifiers** (`fid`s) that it uses for subsequent open, read, write, stat, and remove requests.

The basic interaction is:

```text
client                                  9P server
  │ Tversion(msize, "9P2000.L")           │
  │───────────────────────────────────────>│
  │ Rversion                               │
  │<───────────────────────────────────────│
  │ Tattach(root_fid, identity, tree)      │
  │───────────────────────────────────────>│
  │ Rattach(root_qid)                      │
  │<───────────────────────────────────────│
  │ Twalk(root_fid, file_fid, ["a","b"])  │
  │───────────────────────────────────────>│
  │ Rwalk([qid_a, qid_b])                  │
  │<───────────────────────────────────────│
  │ Tlopen(file_fid, O_RDWR)               │
  │ Tread/Twrite(file_fid, offset, count)  │
  │ Tfsync(file_fid)                       │
  │ Tclunk(file_fid)                       │
```

The original Plan 9 documentation describes 9P as operating on byte-sequence files and using stateful fids; see the [Plan 9 system paper](https://9p.io/sys/doc/9.html) and [9P introduction](https://9p.io/magic/man2html/5/0intro).

### 1.1 Wire concepts

- Requests and replies have a size, type, and request tag.
- Tags allow multiple requests to be outstanding on one connection.
- `Tflush` attempts to cancel an in-flight request by tag.
- `Tversion` negotiates a protocol dialect and maximum message size (`msize`).
- A `fid` is a client-chosen, connection/session-scoped handle to a server object.
- A `qid` is the server's object identity: type, 32-bit version, and 64-bit path value.
- `qid.path` should remain a stable object/inode identity across rename.
- `qid.version` should change when cache-relevant object content changes.
- Reads and writes carry explicit 64-bit file offsets and 32-bit counts.
- `iounit` tells a client the largest transfer the server wants handled as one unit.

The official [Plan 9 stat documentation](https://9p.io/magic/man2html/5/stat) defines the `qid`, attributes, and all-or-nothing attribute changes; [read/write](https://9p.io/magic/man2html/5/read) defines byte-offset I/O.

### 1.2 Dialects

| Dialect | Intended environment | Relevance |
| --- | --- | --- |
| 9P2000 | Plan 9 base protocol | Small and elegant, but not enough Linux/POSIX metadata and operations |
| 9P2000.u | Unix-oriented extension | Historical Linux extension; superseded for new Linux work |
| 9P2000.L | Linux VFS-oriented extension | Recommended baseline for a Linux-mounted w9pt filesystem |

9P2000.L adds numeric Linux errors and operations that map more directly to Linux VFS. The [diod protocol description](https://github.com/chaos/diod/blob/master/protocol.md) is the clearest implementer-oriented reference.

### 1.3 Relevant 9P2000.L operations

| Area | Operations |
| --- | --- |
| Session/navigation | `version`, `auth`, `attach`, `walk`, `flush`, `clunk` |
| Open/create | `lopen`, `lcreate`, `mkdir`, `mknod`, `symlink` |
| Data | `read`, `write`, `readdir`, `fsync` |
| Metadata | `getattr`, `setattr`, `statfs`, `readlink` |
| Namespace | `rename`, `renameat`, `remove`, `unlinkat`, `link` |
| Extended attributes | `xattrwalk`, `xattrcreate` |
| Locking | `lock`, `getlock` |
| Errors | `lerror` with Linux errno values |

This is enough for a large POSIX-like subset. It does not mean every Linux application will work: the protocol and Linux v9fs client have feature and caching limitations.

### 1.4 Linux client behavior

Linux includes the v9fs client and supports TCP, Unix/fd, virtio, RDMA, and other transports. The current [Linux 9P documentation](https://www.kernel.org/doc/html/latest/filesystems/9p.html) lists three important caching choices:

- `cache=none`: minimizes client caching and is the safest starting point for multiple clients.
- `cache=mmap`: enables read-ahead and writeback needed for memory mapping.
- `cache=loose`/`fscache`: may retain stale data because cached values are not necessarily revalidated with the server.

The protocol has no NFSv4-style delegations or callback-based cache invalidation. For the first correctness milestone, use `cache=none` or an explicitly controlled single-client cache mode, then add caching tests.

### 1.5 9P benefits for this project

- Much smaller protocol and state machine than NFSv4.
- Direct byte-offset read/write operations fit an extent-based backend.
- Stateful fids naturally represent open files and open-unlinked objects.
- A stable inode ID maps naturally to `qid.path`.
- `Tfsync` can be defined as the exact object-store durability barrier.
- Linux has an in-kernel client, so a new kernel filesystem is unnecessary.
- User-space servers are normal in the 9P model.

### 1.6 9P costs and limitations

- Linux is the primary stock 9P2000.L client; client portability is much narrower than NFS.
- Metadata-heavy workloads are chatty and amplify S3 latency without a server cache.
- Standard sessions do not survive a lost connection/server restart; fids are gone and stock clients normally need a remount.
- Client caching/coherence choices are weaker than mature NFS implementations.
- Base 9P does not encrypt traffic. Authentication is mechanism-dependent, and many Linux deployments trust numeric identities.
- Supplementary groups and rich ACL behavior are awkward.
- Some Linux operations have no stock protocol equivalent or lose flags/semantics.

### 1.7 9P over WebSocket or HTTP

9P over WebSocket exists in multiple current implementations. There is no broadly adopted standard for mapping stateful 9P RPCs onto independent ordinary HTTP requests.

#### Existing WebSocket implementations

**ZeroFS**

- Its Web UI exposes a `/ws/9p` endpoint.
- Browser/WASM builds of `ninep-client` connect with the browser `WebSocket` API.
- Every outgoing 9P request is sent as one binary WebSocket message.
- Every incoming binary WebSocket message is delivered as one complete 9P response.
- Text frames are rejected/ignored; the 9P bytes inside the binary message retain their normal little-endian size/type/tag header.
- The endpoint serves the ZeroFS private `9P2000.L.Z` dialect as negotiated by its client.

The relevant source is [ZeroFS's WebSocket 9P handler](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/webui.rs) and [browser WebSocket transport](https://github.com/Barre/ZeroFS/blob/main/zerofs/ninep-client/src/web_transport.rs).

**v86**

[v86's filesystem documentation](https://github.com/copy/v86/blob/master/docs/filesystem.md) supports a 9P WebSocket proxy for the browser-hosted emulator. Its convention is also one complete request/reply per binary WebSocket message. A proxy forwarding to a TCP 9P server reads the first four bytes of a reply to learn the full 9P message size, buffers that many stream bytes, then sends one binary WebSocket message.

**Wanix**

[Wanix](https://github.com/tractordev/wanix) imports remote namespaces from `ws://`/`wss://` URLs using 9P. Its [Go WebSocket handler](https://github.com/tractordev/wanix/blob/main/misc/ws9p/ws9p.go) bridges binary WebSocket messages to a stream-oriented 9P2000.L server. It buffers server output by the four-byte 9P length header before emitting a reply message.

These are implementation conventions, not a registered 9P WebSocket subprotocol. Dialect, authentication, endpoint path, maximum frame size, and reconnect behavior differ.

#### Recommended WebSocket framing for w9pt

```text
WebSocket connection = one 9P session

client -> server binary message:
  exactly one complete T-message

server -> client binary message:
  exactly one complete matching R-message
```

Validation rules should include:

1. Reject text messages.
2. Require at least seven bytes: size, message type, and tag.
3. Decode the first four bytes as the little-endian 9P message length.
4. Require declared size to equal the binary WebSocket message length.
5. Enforce both a hard server limit and the negotiated `msize`.
6. Bound outstanding tags, fids, pending writes, and response queues.
7. Preserve 9P tag concurrency; replies need not be emitted in request order when operations complete independently.
8. On WebSocket close, release all connection-scoped fids, locks, and in-flight request state.

A named `Sec-WebSocket-Protocol`, for example `w9pt.9p2000l.v1`, would make dialect/framing intent explicit, although the inspected ZeroFS endpoint does not appear to negotiate one.

#### Why ordinary HTTP request/response is a poor direct fit

9P is session-oriented:

- fids belong to a connection/session;
- tags correlate concurrent requests and replies;
- `Tflush` cancels an earlier tag;
- open files and locks have live state;
- request ordering can affect mutations.

Mapping each 9P request to a separate `POST` would require a session token, sticky routing or shared session state, explicit sequencing, cancellation routing, timeout/retry rules, and protection against a proxy replaying a non-idempotent request. HTTP specifications also warn application protocols not to infer a stable connection or ordering relationship between separate HTTP requests; see [RFC 9205](https://www.rfc-editor.org/rfc/rfc9205.html).

HTTP can still be used in three different senses:

- WebSocket starts as an HTTP Upgrade, then becomes a persistent full-duplex message transport.
- HTTP `CONNECT` can tunnel an ordinary TCP 9P byte stream; this is tunneling, not an HTTP mapping.
- A custom HTTP file/object API can expose similar operations, but it is no longer 9P and should use HTTP-native idempotency and resource semantics.

I found no widely used 9P-over-HTTP/1.1 REST binding. WebTransport or HTTP/2/3 extended CONNECT could carry 9P in the future, but no established interoperable binding was found in this survey.

#### Browser and Linux interoperability

A browser cannot make the host Linux VFS mount a WebSocket directly. Options are:

- use a JavaScript/WASM 9P client and expose a browser API/virtual namespace;
- run a local WebSocket-to-TCP proxy, then mount the proxy's TCP endpoint with Linux v9fs;
- build a FUSE client that speaks WebSocket;
- use v86, whose guest virtio-9p device forwards through a browser WebSocket.

For w9pt's browser/OPFS use case, a WASM 9P client over `wss://` is viable. For native Linux, direct TCP/Unix/virtio 9P is simpler and avoids WebSocket overhead unless traversal through an HTTP reverse proxy is required.

#### Security and operations

- Use `wss://`, not plaintext `ws://`, outside a trusted local environment.
- Authenticate during the HTTP upgrade with a short-lived token, mTLS at the proxy, or another explicit mechanism; TLS alone authenticates only the server by default.
- Validate `Origin`; do not copy permissive examples that accept every origin.
- Bind the authenticated principal to the 9P attach/session rather than trusting a browser-supplied numeric UID.
- Set reverse-proxy idle timeouts and maximum message sizes to support negotiated 9P traffic.
- Apply backpressure using the WebSocket buffered amount and bounded queues.
- Do not assume reconnect restores fids. Stock 9P loses the session; resumption requires a custom dialect/client like ZeroFS's extensions.
- WebSocket transport does not make 9P traffic cacheable by HTTP CDNs; it remains live bidirectional RPC.

### 1.8 Stateful 9P with stateless WebSocket nodes

A WebSocket node cannot be literally stateless while a socket is open. It must own at least the transport connection, receive buffer, output queue/backpressure, authentication context, and an upstream route. The useful architectural goal is narrower:

> Keep no durable filesystem/session state in the WebSocket tier, so a client can reconnect through any node and rebuild its 9P state against an authoritative backend.

This is feasible, but standard 9P2000.L does not supply the recovery operations.

#### Existing implementation comparison

| Implementation | Where fids/open state live | WebSocket tier stateless? | Reconnect through another node? |
| --- | --- | --- | --- |
| ZeroFS Web UI endpoint | Per-WebSocket `NinePHandler` in the ZeroFS process; local fid table, open-handle guards, locks, in-flight registry | No—the endpoint and semantic server are co-located and connection-stateful | Its private client/dialect can create a new session and replay/rebind much state; not transparent for every resource |
| v86 WebSocket proxy | Backend TCP 9P connection owns the actual session; proxy only frames/forwards bytes | Semantically thin, but connection-affine | No standard session restoration; lost proxy/upstream means lost fids/remount |
| Wanix `ws9p` handler | An in-process 9P server is instantiated for the WebSocket stream | No | No durable/migratable session mechanism found |

ZeroFS's current handler stores `HashMap<u32, FidSlot>` per session. Each slot includes inode/path identity, credentials, open state, an open-inode pin, replay marker, and lock guard. Disconnect releases the slots and their locks. See [ZeroFS `NinePHandler`](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/ninep/handler.rs).

#### ZeroFS out-of-the-box verdict

This matrix was rechecked against ZeroFS v2.3.2 on 2026-09-01:

| Capability | Out of the box? | Qualification |
| --- | --- | --- |
| Stateful 9P connection | Yes | Every TCP/Unix/WebSocket connection owns a local session/fid table |
| Reconnect after server/network loss | Yes, private clients only | `zerofs mount`, native client, client libraries, and Web UI use `9P2000.L.Z` |
| Restore linked open files after reconnect | Yes | Client rebinds by stable inode ID and reopens with current authorization checks |
| Retry ambiguous mutations safely | Yes, bounded | Operation IDs/result ledger; automatic retry horizon is 120 seconds |
| Reacquire advisory byte-range locks | Attempted | Lock is not continuous; another client can acquire it during the gap |
| Recover open-unlinked file descriptor | No | It is connection-local and becomes stale |
| Stock Linux v9fs session restoration | No | Plain `9P2000.L` requires remount after disconnect |
| Multi-endpoint leader/standby failover | Yes | Fixed configured pair; one active writer, one standby—not arbitrary stateless workers |
| Stateless WebSocket edge/service | No | WebSocket endpoint creates `NinePHandler` and fid/open/lock state in the serving process |
| Active WebSocket migration between nodes | No | Failure closes the connection; recovery creates a new session and replays it |
| External durable session store | No | No Redis/database service stores complete fid/open/lock/in-flight state |
| Stateless CSI node plugin | No | It owns child FUSE client processes; plugin restart breaks its published mounts |
| Disposable local data cache | Yes | Durable filesystem state is in object storage, but live sessions are not |

Thus ZeroFS is **durably object-backed and reconnectable**, not stateless at the live protocol/node layer. Its HA pair hides many gateway failures from private clients through reconstruction, but does not hand an active connection or complete session state from one stateless node to another.

#### ZeroFS extension model

ZeroFS demonstrates how 9P can be extended for reconnection. Its own clients negotiate the exact private version string `9P2000.L.Z`; stock v9fs negotiates `9P2000.L`. The private dialect is all-or-nothing rather than feature-bit negotiated. See [ZeroFS 9P extensions](https://github.com/Barre/ZeroFS/blob/main/documentation/src/app/9p-extensions/page.mdx).

The relevant additions are:

1. **Mutation operation envelope**
   - Reorder-sensitive mutations carry a 128-bit operation ID, attempt flags, and writer epoch.
   - A server retry ledger serializes duplicates and retains the original result.
   - A retry unknown to the ledger fails closed instead of being treated as a new mutation.
   - This prevents a lost reply followed by reconnect/retry from applying `write`, `rename`, `unlink`, or create twice.

2. **Fid rebinding by stable inode ID**
   - `Trebind` creates a fresh-session fid for a still-linked inode.
   - It retains attach-root and credential context and rechecks current reachability/authorization.
   - Rewalking the old path is unnecessary, so an open file renamed during disconnection can recover.

3. **Open-state replay**
   - A rebound fid can be marked as expected to reopen.
   - The actual reopen rechecks permissions and inode liveness before installing an open-handle pin.

4. **Durability lineage**
   - A client obtains a lineage token and writer epoch.
   - A private fsync variant succeeds only if the client's mutations are durable in the still-authoritative lineage; a stale leader returns `ESTALE`.

5. **Compound/attribute operations**
   - Private messages combine walk/getattr, readdir/getattr, open/read, and create/getattr sequences, reducing round trips over higher-latency WebSockets.

The design still has explicit gaps:

- An open-unlinked fid cannot be recovered after connection loss because the old session's final inode pin is gone.
- A byte-range lock can be reacquired, but another session can acquire it during the disconnect gap; this is not uninterrupted fencing.
- Standard Linux v9fs cannot use the extensions.
- Client, server, Web UI, and HA peers must upgrade together because `.Z` has no feature-by-feature negotiation.

#### Three scalable architectures

**A. Connection-pinned stateless proxy**

```text
browser --WebSocket--> edge proxy --TCP/Unix 9P--> one backend connection
```

The edge understands only WebSocket and 9P message lengths. It keeps no fid table or filesystem metadata. A load balancer chooses any edge during upgrade; that edge remains pinned for the life of the socket. No sticky cookie is needed after upgrade because the connection itself is the affinity.

Advantages:

- simplest horizontally scalable edge;
- works with existing 9P servers;
- isolates WebSocket/TLS/Origin/auth handling from storage;
- easy rolling drain: stop accepting connections, then close/reconnect remaining sessions.

Limitations:

- an edge or upstream connection failure destroys the 9P session;
- active sockets cannot move between edge nodes;
- standard clients must remount or rebuild from paths;
- the proxy is stateless only across connections, not during one.

This is the v86-style model and the recommended first w9pt milestone.

**B. Stateless edge plus client-driven session reconstruction**

```text
browser client
  - retains fid recipes, stable inode IDs, opens, locks, pending op IDs
          │ reconnect through any edge
          ▼
stateless WebSocket edge
          │ route to authoritative filesystem leader
          ▼
backend
  - durable namespace/inodes
  - idempotency ledger
  - writer epoch/fencing
```

After a disconnect, the client authenticates again, creates a fresh 9P session, reattaches, rebinds/reopens each recoverable fid, reacquires advisory locks, and resends ambiguous mutations with the original operation IDs. The edge remains disposable; recovery state lives in the client and authoritative backend.

Requirements:

- stable inode IDs that are never reused;
- custom version negotiation and rebind messages;
- per-mutation idempotency IDs with a bounded retry/retention horizon;
- backend writer fencing and durable-result lineage;
- client-side session journal and deterministic recovery ordering;
- clear stale outcomes for unlinked files, revoked permissions, deleted inodes, and unrecoverable locks.

This is closest to ZeroFS's model and is the recommended long-term w9pt direction if browsers need resilient sessions.

**C. External durable session service**

```text
WebSocket edge -> session actor/service -> filesystem backend
                     │
                     └── durable/replicated fid, open, lock, in-flight state
```

The edge supplies a signed session token and forwards messages to a session actor selected by consistent routing. Another edge can resume the same actor. Active actor migration requires sequence numbers, pause/drain, acknowledgements, leases, and fencing.

This can preserve open-unlinked handles and locks better, but it effectively builds a distributed stateful filesystem-session database. Persisting only a `fid -> inode` map is insufficient; live state includes open pins, lock ownership, dirty write buffers, cancellation tags, reply deduplication, credentials, attach roots, and GC protection.

Use this only if uninterrupted session migration is a hard requirement. It is substantially more complex than reconstructing recoverable state from the client.

#### Proposed w9pt extension shape

Because the browser client and server are both under w9pt's control, define a private dialect rather than pretending to remain stock-compatible:

```text
Tversion "9P2000.L.weft1"
Rversion "9P2000.L.weft1"
```

Minimum additions:

| Extension | Purpose |
| --- | --- |
| Mutation envelope `{op_id, first/retry, writer_epoch}` | Exactly-once-visible outcome across ambiguous reconnects |
| `Tgetsession`/`Rgetsession` | Return session capabilities, recovery horizon, lineage, and server generation |
| `Trebind(inode, root, credentials, flags)` | Recreate fids without depending on old paths |
| Durable/recoverable-open flag | Distinguish handles the server can safely restore |
| `Tfsyncdur(token)` | Prove writes survived against the authoritative lineage |
| Optional compound lookup/open/read and readdir-plus | Reduce browser round trips |

Prefer a versioned capability bitset after private-version negotiation instead of ZeroFS's indivisible all-features token if independent extension rollout matters. Unknown mandatory capabilities must fail negotiation before either peer decodes incompatible frames.

#### Edge-node contract

A disposable WebSocket node should own only:

- the open WebSocket and upstream connection/stream;
- bounded request/response queues and backpressure;
- authenticated principal/claims established at upgrade;
- request size validation and observability;
- routing information for the current backend leader/session actor.

It should not own authoritative:

- fid-to-inode state;
- open-unlinked inode pins;
- file locks;
- mutation deduplication outcomes;
- dirty data or durability state;
- filesystem metadata caches required for correctness.

On edge loss, all transient transport state disappears. Recovery succeeds because the client and backend reconstruct it, not because another edge somehow inherits the socket.

#### Load balancing and WebSocket versions

RFC 6455 permits reverse proxies/load balancers to participate in server-side connection handling, but a WebSocket remains a long-lived connection until close. WebSockets over HTTP/2 ([RFC 8441](https://www.rfc-editor.org/rfc/rfc8441.html)) or HTTP/3 ([RFC 9220](https://www.rfc-editor.org/rfc/rfc9220.html)) multiplex the WebSocket as a stream; they do not make the application session stateless or migratable.

Recommended rollout:

1. Begin with one binary 9P message per WebSocket message and a connection-pinned framing proxy.
2. Make all mutations idempotent using client operation IDs before adding automatic reconnect.
3. Add stable-inode fid rebind and deterministic client-side recovery.
4. Add writer lineage/fencing and `fsync` verification.
5. Treat open-unlinked handles and locks as stale across reconnect initially.
6. Consider a durable session actor only if real workloads require uninterrupted recovery for those resources.

## 2. What S3 contributes

Amazon S3 is a key/object service, not a filesystem. Current S3 nevertheless has several properties that make it a viable durable substrate:

- Strong read-after-write consistency for `PUT`, overwrite, and `DELETE` in all Regions.
- Strongly consistent `GET`, `HEAD`, and `LIST` after successful writes.
- Atomic updates to a **single object key**: readers see old or new content, never a partial object.
- Conditional writes with `If-None-Match: *` and ETag-based `If-Match`.
- Byte-range `GET` for efficient partial reads.
- Multipart upload for large immutable objects.
- Optional versioning and delete markers for recovery/history.
- High parallel throughput and S3-compatible alternatives outside AWS.

AWS documents the consistency and single-key atomicity in [What is Amazon S3?](https://docs.aws.amazon.com/AmazonS3/latest/userguide/Welcome.html), and conditional compare-and-set behavior in [Conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html).

### 2.1 General-purpose bucket model

General-purpose buckets are flat. Slash-delimited “folders” are prefixes inferred by tools; they are not directories. Object keys are case-sensitive UTF-8 sequences of at most 1,024 bytes. See [Naming Amazon S3 objects](https://docs.aws.amazon.com/AmazonS3/latest/userguide/object-keys.html).

An object can be range-read but cannot be randomly overwritten. A new `PUT` or completed multipart upload replaces the object. Existing user metadata cannot be changed independently; changing it requires copying/replacing the object. A “rename” is copy to a new key followed by delete of the old key, so it is not one atomic namespace operation. See [Copying, moving, and renaming objects](https://docs.aws.amazon.com/AmazonS3/latest/userguide/copy-object.html).

### 2.2 Directory buckets and S3 Express One Zone

Directory buckets now provide hierarchical indexing and two operations that improve file-like workloads:

- `RenameObject` renames one object within the same directory bucket without copy/delete.
- `PutObject` with `WriteOffsetBytes` can append at exactly the current end of an object.

Both are limited to S3 Express One Zone directory buckets. Append is not arbitrary overwrite, and `RenameObject` does not provide an atomic recursive directory-tree rename. Current Mountpoint documentation says directory rename is unsupported for every S3 bucket type. These additions improve a thin adapter but do not supply inodes, links, permissions, random writes, locks, or multi-object transactions. See [RenameObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_RenameObject.html) and [Appending data to directory-bucket objects](https://docs.aws.amazon.com/AmazonS3/latest/userguide/directory-buckets-objects-append.html).

### 2.3 S3 capability gap

| Filesystem requirement | Native S3 behavior | Extra filesystem layer required? |
| --- | --- | --- |
| Strong lookup after create/delete | Strong `GET`/`LIST` consistency | No, for one key |
| Random read | Range `GET` | No |
| Random overwrite | No; replace whole object | Yes, extent/chunk indirection |
| Append | Directory buckets only; end offset only | Usually |
| Truncate | Replace object | Yes for efficient mutable files |
| Atomic create-if-absent | Conditional `PUT If-None-Match: *` | No, for one key |
| Compare-and-swap | Conditional `PUT If-Match: ETag` | No, for one key |
| Stable inode across rename | Key is the native identity | Yes |
| Empty directory | Prefix is not a directory in general-purpose buckets | Yes/marker emulation |
| Atomic file rename | Copy/delete in general buckets; single-object rename in directory bucket | Yes for portable semantics |
| Atomic non-empty directory rename | No | Yes |
| Cross-directory atomic rename/replace | No multi-key transaction | Yes |
| Hard link/link count | No | Yes |
| Symlink | No native type | Yes |
| UID/GID/mode/timestamps | No POSIX inode model | Yes |
| Mutable xattrs | User metadata replacement rewrites/copies object | Yes |
| Open-unlink lifetime | Delete removes key visibility independently of open readers | Yes |
| Advisory byte-range locks | No | Yes, live lock manager |
| `fsync` for file plus namespace | One object can become durable; no multi-object commit | Yes |
| Coherent multi-client page cache | No filesystem cache protocol | Yes |
| Quota/`statfs` | Bucket effectively elastic | Configured accounting required |

## 3. Why direct `path -> key` mapping is not a full filesystem

A thin server is tempting:

```text
9P walk /a/b       -> HEAD s3://bucket/a/b
9P readdir /a      -> LIST prefix=a/ delimiter=/
9P read /a/b       -> GET range from key a/b
9P create /a/c     -> PUT key a/c
9P remove /a/b     -> DELETE key a/b
9P rename /a/b /x  -> COPY a/b to x; DELETE a/b
```

This is useful for data lakes, model/checkpoint access, media, backups, logs, and create-once outputs. It is not a general filesystem:

- `qid.path` derived from a key changes on rename; derived from a hash changes on content update.
- A directory exists only because keys share a prefix, so empty directories disappear.
- Renaming a directory requires copying/deleting every descendant and exposes intermediate states.
- Partial writes require downloading/reconstructing/re-uploading an entire object.
- Two metadata operations cannot be committed atomically.
- Hard links cannot be represented because path and object identity are the same thing.
- `unlink` cannot preserve an already-open inode cleanly.
- `chmod` or `chown` can require rewriting/copying a large object when stored as object metadata.
- Per-file locks do not exist.
- Direct S3 writers can bypass the 9P server's permissions, locks, generations, and invariants.

AWS's open-source [Mountpoint for Amazon S3](https://github.com/awslabs/mountpoint-s3) deliberately chooses this limited model. It optimizes large reads and sequential creation/overwrite, rejects unsupported mutations, lacks stable inodes, mutable permissions, xattrs, locks, general random writes, and directory rename, and explicitly warns against editing existing files. That is the appropriate behavior for a transparent thin adapter.

## 4. Viable architecture choices

### 4.1 Transparent S3 gateway

```text
9P server -> one S3 object per visible file
```

Best for:

- existing buckets that must remain directly browsable through S3;
- read-mostly workloads;
- sequentially created outputs;
- applications that tolerate explicit `EOPNOTSUPP`.

Verdict: simplest, but cannot meet the requested full-operation goal.

### 4.2 Single gateway with local metadata database

```text
9P server
  ├── SQLite/RocksDB/local WAL: inodes, directories, extents, locks
  └── S3: immutable file chunks/segments
```

This can implement full semantics while one server is authoritative. SQLite transactions make namespace operations straightforward, and S3 holds durable bulk data. The problems are metadata durability, restore, and failover: losing the local database loses the filesystem even if every data chunk remains in S3. Continuously uploading database snapshots/WAL helps recovery but makes synchronous high availability difficult.

Verdict: good prototype and single-node product if metadata backup/recovery is explicit; not an S3-only HA design.

### 4.3 Object-backed metadata engine plus immutable segments

```text
9P server
  ├── local RAM/disk cache and write journal
  ├── object-backed metadata LSM/B-tree
  └── S3
       ├── immutable packed data segments
       ├── immutable metadata tables/pages
       └── current manifest/root + writer fencing records
```

File data is split into fixed logical extents. Modified extents are written as new immutable frames and grouped into large segment objects. Metadata maps inode/extent indexes to segment byte ranges. A commit uploads data first, uploads metadata next, then atomically advances the durable metadata manifest/root.

S3 conditional writes can create immutable blobs once and compare-and-swap a small root/fencing object. A single active writer serializes POSIX namespace operations. Garbage collection later removes obsolete metadata and data segments.

Verdict: best S3-only design, but it is a substantial database/filesystem project. ZeroFS uses this model.

### 4.4 External transactional metadata service

```text
9P gateways
  ├── DynamoDB/PostgreSQL/Raft service: namespace, inode, lease, locks
  └── S3: immutable extents/segments
```

This is the common distributed-filesystem split. Transactions, conditional updates, leases, and multiple gateways are easier, while S3 remains the bulk-data layer. It adds another durable service and operational dependency.

Verdict: most practical for multi-writer HA if “S3-backed” does not mean “S3 is the only durable service.”

## 5. Existing direct precedent: ZeroFS

[ZeroFS](https://github.com/Barre/ZeroFS) already implements the proposed pair. As checked on 2026-09-01, the repository is active, written primarily in Rust, has an AGPL-3.0 open-source license plus a commercial license, and advertises S3-compatible, Azure Blob, GCS, and local backends.

Its published architecture is especially relevant:

- One userspace filesystem core serves 9P, NFS, NBD, and a web UI.
- File contents use 32 KiB logical extents.
- Extents are compressed and encrypted into frames.
- Frames accumulate into immutable segment objects up to 256 MiB.
- Reads locate a frame and use ranged object-store GETs; adjacent reads coalesce.
- Inodes, directory entries, link counts, extent pointers, tombstones, orphan records, and segment accounting live in an object-backed LSM tree.
- A local memory/disk cache stores raw encrypted/compressed object parts.
- Metadata manifests never reference a data segment that has not been uploaded.
- A writer epoch and conditional object creation provide fencing.
- Segment garbage collection deletes empty segments and repacks fragmented ones.
- 9P `fsync` waits until data reaches stable storage.

The project reports pjdfstest, xfstests, kernel-build, stress, crash-recovery, and Jepsen-style HA tests. These are project claims, not an independent audit, but the test categories are the right ones.

### 5.1 ZeroFS block/extent splitting implementation

ZeroFS contains a complete block-splitting data path in its main repository. It uses fixed, file-relative **32 KiB extents**, not content-defined chunking and not one continuous stream across all files.

The defining constant is:

```rust
pub const EXTENT_SIZE: usize = 32 * 1024;
```

For a write covering byte range `[offset, offset + length)`, the implementation calculates:

```text
start_extent = offset / 32 KiB
end_extent   = (offset + length - 1) / 32 KiB
```

The metadata key is logically `(inode_id, extent_index)`, so extent zero of file A is unrelated to extent zero of file B. No extent crosses a file boundary.

#### Write path

The current source performs these steps:

1. Determine every 32 KiB extent touched by the write.
2. Skip reading old data for a full-extent overwrite or an extent beyond EOF.
3. For a partial overwrite, fetch the old full extent and apply the changed byte range in memory.
4. Treat a missing extent as a zero-filled sparse hole.
5. If the resulting full extent is all zero, delete its extent pointer so it remains a hole.
6. Compress each nonzero 32 KiB extent independently using the configured LZ4 or Zstd codec.
7. Encrypt it with XChaCha20-Poly1305 using a random 24-byte nonce.
8. Bind the encrypted frame to `(segment ID, frame index, inode ID, extent index)` as AEAD associated data, preventing a frame from being moved to another logical block undetected.
9. Append `[sealed_length: u32][sealed_frame]` to the current in-memory segment.
10. Store a 32-byte `FrameLoc` metadata value for `(inode, extent)` containing segment epoch/counter, frame index, byte offset, and byte length.
11. Debit the superseded frame from its old segment's live-byte accounting.
12. Seal/upload the open segment in the background when its packed bytes reach 256 MiB, or synchronously at the `fsync`/flush barrier.

The implementation is in:

- [`EXTENT_SIZE` and filesystem constants](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/fs/mod.rs)
- [`ExtentStore` overview](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/fs/store/extent/mod.rs)
- [extent write, partial RMW, truncate, zero-range, and segment sealing](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/fs/store/extent/write.rs)

Conceptually:

```text
file inode 42

logical file bytes
  [extent 0: 32 KiB][extent 1: 32 KiB][extent 2: 32 KiB]...
            │                  │                  │
            ▼                  ▼                  ▼
      compress+encrypt   compress+encrypt   sparse hole
            │                  │
            └──────── frames appended ────────────┐
                                                  ▼
S3 segment object
  [frame for (42,0)][frame for (42,1)][other files' frames...]
  [encrypted reverse-map directory]
  [64-byte footer]

metadata LSM
  (42,0) -> FrameLoc(segment S, byte range X)
  (42,1) -> FrameLoc(segment S, byte range Y)
  (42,2) -> absent = zero/hole
```

Different files' frames can share the same **physical segment object**, but the logical 32 KiB extents and metadata remain file/inode scoped. This combines small random-write units with much larger S3 objects, avoiding one S3 request/object per 32 KiB block.

#### Segment format

The segment implementation is self-describing:

```text
[frame_0][frame_1] ... [frame_n]   # packed without padding
[encrypted reverse-map directory]  # frame -> inode/extent
[64-byte plaintext footer]          # version, IDs, offsets, CRC
```

Segment object keys use a writer epoch and monotonically allocated counter, sharded across 256 prefixes:

```text
segments/<counter-low-byte>/<epoch>/<counter>
```

The code is in [segment format and `FrameLoc`](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/segment.rs), [object-store segment reads/writes](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/segment_store.rs), and [compression/encryption codec](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/frame_codec.rs).

#### Read path

For a read, ZeroFS:

1. Converts the requested file range to extent indexes.
2. Scans metadata for each `(inode, extent)` `FrameLoc`.
3. Returns zeroes for absent extent keys.
4. Reads new/unflushed frames directly from the in-memory open or sealing buffers.
5. Otherwise issues a ranged S3 GET for the frame bytes.
6. Coalesces consecutive frames from the same segment into one ranged GET.
7. Authenticates, decrypts, decompresses, and validates that each result is exactly 32 KiB.
8. Slices the first/last extent to the byte range requested by the client.
9. Starts bounded 8 MiB logical read-ahead after detecting sequential access.

See [extent read and range coalescing](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/fs/store/extent/read.rs).

#### It is not content deduplication

ZeroFS's extents are not named by a plaintext content hash, and its segment object keys are epoch/counter identities. Frame encryption uses random nonces, so identical plaintext extents do not naturally produce identical stored frames. The repository's `dedup.rs` is an idempotency/retry ledger for mutation operation IDs, not data-block deduplication.

When a block is overwritten, the new extent version is appended elsewhere and the old frame becomes reclaimable garbage. Segment GC tracks live bytes, deletes fully dead segments, and repacks live frames from fragmented segments. This is log-structured copy-on-write storage rather than SteamPipe-style chunk reuse.

This block implementation was inspected on the `main` branch on 2026-09-01 and may evolve.

### 5.2 Can ZeroFS be used as a library?

There are two different answers depending on whether “library” means an in-process storage engine or a client SDK.

#### In-process Rust storage engine: technically yes, but not a polished embedding API

The current v2.3.2 package has both `lib` and `bin` Cargo targets. Its [`lib.rs`](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/lib.rs) publicly exports the filesystem, database, segment, codec, replication, and related modules. The core type is [`zerofs::fs::ZeroFS`](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/fs/mod.rs), and filesystem operations such as lookup, read, write, create, rename, link, remove, and setattr are public inherent methods.

A current release can be resolved as a pinned Git dependency:

```toml
[dependencies]
zerofs = { git = "https://github.com/Barre/ZeroFS.git", tag = "v2.3.2" }
```

This dependency form and its `lib` target were verified with Cargo metadata on 2026-09-01. Pin a release tag or exact revision; following `main` would expose a filesystem format and API dependency to unreviewed changes.

However, the exposed core is shaped like internal server machinery rather than a documented embedding SDK:

- The convenient production setup, object-store parsing, 9P server, NFS server, RPC server, and CLI application modules are private.
- `run_cli` is public but hidden, and its source comment says it is public only so the thin binary can enter the library-owned implementation graph.
- The public `ZeroFS::try_new` constructor requires a pre-opened SlateDB handle plus metrics, write policy, lease, replicator, dedup ledger, lineage proof, object tracer, object store, frame codec, and seal configuration.
- The easy in-memory constructors are compiled only for tests.
- The extent and segment types are public but depend on ZeroFS metadata keys, transactions, writer coordination, encryption keys, live-byte accounting, flushing, and GC invariants.
- There is no documented `ZeroFsBuilder`/`open_s3`-style public constructor for an embedded application.
- The private `ninep` module means a library consumer cannot simply instantiate the project's 9P server through a supported public API.

Therefore the current core can be embedded by an experienced Rust consumer willing to construct its internals, pin/fork the code, and absorb API changes, but the repository does not present this as the primary supported integration route. This is an inference from the current source/API shape, not an explicit upstream compatibility promise.

The current GitHub release line is v2.3.2, while `cargo search zerofs` currently returns the older `zerofs = 0.19.1` registry release. That crates.io package is materially behind the current format/API and should not be assumed compatible with current documentation. Use one pinned release source consistently.

#### Remote Rust client: yes, explicitly supported

[`zerofs-client`](https://crates.io/crates/zerofs-client) is the intended library-style application API. It is a separately versioned, documented async Rust crate that connects to a running ZeroFS server over the private `9P2000.L.Z` dialect. Its API includes:

- path-based read, write, append, stat, rename, mkdir, remove, hard-link, symlink, chmod/chown, truncate, and sync operations;
- positioned `File::read_at`/`write_at` and `sync_all`;
- incremental directory handles;
- Tokio `AsyncRead`/`AsyncWrite`/`AsyncSeek` adapters;
- reconnect and HA target handling.

Example:

```toml
[dependencies]
zerofs-client = "0.3.0"
```

```rust
use zerofs_client::{Client, OpenOptions};

let fs = Client::connect("unix:/run/zerofs/9p.sock").await?;
let file = fs
    .open("/data/file.bin", OpenOptions::read_write().create(true))
    .await?;
file.write_at(4096, b"changed bytes").await?;
file.sync_all().await?;
file.close().await;
```

See the [`zerofs-client` README](https://github.com/Barre/ZeroFS/blob/main/zerofs/zerofs-client/README.md) and [docs.rs API](https://docs.rs/zerofs-client/latest/zerofs_client/).

#### Protocol and foreign-language libraries

The repository also publishes/separates:

| Package | Purpose | Server/storage engine? |
| --- | --- | --- |
| [`ninep-proto`](https://crates.io/crates/ninep-proto) | 9P2000.L wire types/codecs | No |
| [`ninep-client`](https://crates.io/crates/ninep-client) | Lower-level async 9P client with reconnect | No |
| [`zerofs-client`](https://crates.io/crates/zerofs-client) | High-level path/file/directory client | No |
| `zerofs-ffi` | UniFFI bindings for the client to Python, Node/TypeScript, and Go | No; `publish = false` as a Rust crate |

The FFI package is explicitly client-only. Its [README](https://github.com/Barre/ZeroFS/blob/main/zerofs/zerofs-ffi/README.md) describes Python wheels, Node/TypeScript native packages, and Go/cgo generation. These bindings connect to a server; they do not embed the S3 storage engine.

All these ZeroFS crates and bindings use AGPL-3.0. ZeroFS also advertises a commercial license. In-process use, modified service deployment, and distribution each need license review; this note is not legal advice.

#### Recommendation for w9pt

| Goal | Recommended approach |
| --- | --- |
| Evaluate ZeroFS behavior | Run it externally as a reference and benchmark only |
| Implement w9pt's primary product | Build native embeddable protocol/session, semantic-core, and backend APIs |
| Embed ZeroFS's current core | Technically possible by pinning/forking, but conflicts with w9pt's clean API and Apache-2.0 licensing goals |
| Reuse only its block splitter | Reimplement the small fixed-extent algorithm; importing `ExtentStore` pulls in most core invariants |
| Reuse a 9P server library without ZeroFS | Prefer an independently supported library such as Apache-2.0 `hugelgupf/p9` |

Because w9pt's defining scope is in-process embedding, the ZeroFS sidecar/client route is not the product architecture. It remains useful for behavioral comparison, performance baselines, and failure-test ideas. w9pt should expose its own stable builders and traits rather than coupling applications to ZeroFS internals.

### 5.3 Can w9pt copy only the block-splitting logic without AGPL?

This section is a technical licensing risk summary, not legal advice. Copyright/derivative-work rules vary by jurisdiction and facts; obtain qualified counsel before choosing a release license.

“Free library” is ambiguous:

- If it means **free/open-source under AGPL-3.0**, ZeroFS code can be copied and modified subject to AGPL compliance.
- If it means **free of AGPL obligations**, allowing an MIT/Apache/proprietary library, ZeroFS code should not be copied, translated, or closely adapted without a separate license from the copyright holder.

ZeroFS's repository [`LICENSE`](https://github.com/Barre/ZeroFS/blob/main/LICENSE) is AGPL-3.0, and the project advertises a separate commercial-license option.

#### Practical classification

| Approach | Likely licensing result |
| --- | --- |
| Copy `extent/write.rs`, `segment.rs`, or portions into w9pt | The copied/modified work remains AGPL-covered |
| Translate the Rust implementation into another language | A translation/adaptation can still be a derivative work; changing language does not remove AGPL |
| Rename identifiers, reorganize functions, or lightly rewrite copied code | Still high derivative-work risk |
| Link/import the ZeroFS crate into one executable | GNU guidance treats linked/shared-address-space modules as one combined program; plan for AGPL obligations |
| Modify and operate an AGPL network service | AGPL section 13 requires offering corresponding source to remote users of the modified version |
| Run an unmodified ZeroFS process and talk through ordinary 9P | More plausibly separate programs; ZeroFS remains AGPL, while the client may be separately licensed, but the exact boundary is fact-specific |
| Independently implement fixed extents from functional requirements without copying expression | Can generally use a separately chosen license under U.S. copyright principles, subject to jurisdiction, patents, and actual independence |
| Obtain ZeroFS's commercial license | Rights depend on the negotiated license; may permit proprietary embedding or reuse |

The [AGPLv3 text](https://www.gnu.org/licenses/agpl-3.0.en.html) defines a modified work as copying or adapting all or part in a manner requiring copyright permission. Section 5 requires a conveyed work based on the program to be licensed as a whole under AGPL, while section 13 adds the remote-network source offer for modified network software. GNU's [license FAQ](https://www.gnu.org/licenses/gpl-faq.en.html) says modules linked in one executable/shared address space are normally a combined program, while separate processes communicating through ordinary pipes or sockets are more likely separate works; the semantics and intimacy of communication still matter.

#### The general algorithm can be reimplemented

The U.S. Copyright Office states that copyright protects a computer program's expression, not its ideas, program logic, algorithms, systems, methods, concepts, or layouts. It also describes clean-room implementations as a long-used way to study functionality and create different code. See [Computer Programs](https://www.copyright.gov/register/tx-programs.html) and the Copyright Office's [software report](https://www.copyright.gov/policy/software/software-full-report.pdf).

The high-level technique is conventional filesystem engineering:

```text
extent_size = chosen fixed size
start_extent = offset / extent_size
end_extent = (offset + length - 1) / extent_size

for each affected extent:
    if write covers full extent:
        use new bytes directly
    else:
        read old extent or zero-filled hole
        apply changed range

    if result is all zero:
        remove mapping (sparse hole)
    else:
        encode a new immutable extent record
        atomically replace the logical extent pointer
```

Implementing this behavior independently is different from copying ZeroFS's Rust expression. A w9pt implementation should make independent choices about:

- extent and segment sizes;
- metadata schema and serialization;
- compression and encryption framing;
- object naming and sharding;
- transaction, flush, and recovery model;
- concurrency and locking structure;
- GC accounting and compaction selection;
- error model, APIs, identifiers, comments, and tests.

Avoid copying or mechanically paraphrasing ZeroFS source, comments, tests, data structures, AAD labels, wire/storage format, function decomposition, or distinctive sequencing. Matching behavior for interoperability can require additional legal analysis.

#### Clean-room caution for this project

This research session inspected ZeroFS source in detail, including its constants, data structures, segment layout, and write/read sequence. It is therefore **not** a strict clean-room environment.

If clean-room independence is important:

1. Have counsel define the process and relevant jurisdictions.
2. Create a behavior-only requirements document based on generic filesystem requirements and independently selected design decisions.
3. Exclude ZeroFS code, comments, tests, and distinctive internal names/format details from that document.
4. Have implementers who have not reviewed ZeroFS source write the code.
5. Develop independent tests from the requirements rather than porting ZeroFS tests.
6. Keep provenance, design-decision, and review records.
7. Perform a patent/contract review; copyright independence does not decide patent rights or every other restriction.

For w9pt, the block-splitting arithmetic itself is small and generic enough that a fresh implementation is preferable to importing the tightly coupled AGPL `ExtentStore`. If exact ZeroFS format or code reuse is required, use AGPL compliantly or negotiate the advertised commercial license.

### 5.4 Important ZeroFS qualifications

- Standard stock v9fs sessions do not reconnect; ZeroFS adds a private `9P2000.L.Z` dialect and custom clients for reconnect and compound operations.
- Its 9P endpoint does not authenticate clients or encrypt traffic; it recommends loopback, controlled networks, VPNs, or authenticated tunnels.
- Numeric UID/GID handling and supplementary groups are limited.
- Advisory locks live in server memory and disappear on restart/takeover; custom clients attempt reacquisition, which is not distributed fencing.
- Extended attributes and POSIX ACLs are not supported over its 9P path.
- Several advanced Linux operations and rename flags are unsupported.
- AGPL network-use obligations or a commercial license must be considered before code reuse.

ZeroFS demonstrates feasibility, but it also demonstrates why “full” needs a written feature contract.

## 6. Recommended w9pt framework design

### 6.1 Layering

```text
embedding application
  ├── owns listener/transport, executor, auth, policy, and lifecycle
  └── accepts a connection or message stream
                         │
                         ▼
┌──────────────────────────────────────────────┐
│ w9pt protocol/session framework              │
│ 9P decode/encode, tags, fids, qids, cancel   │
├──────────────────────────────────────────────┤
│ w9pt filesystem semantic core                │
│ inodes, dirs, attrs, open state, locks,       │
│ transactions, quotas, durability contracts   │
├──────────────────────────────────────────────┤
│ pluggable backend interface                  │
└──────────────────────┬───────────────────────┘
                       ▼
             S3 backend (first implementation)
                       │
                       ▼
                private S3 prefix
```

The host application drives sessions and supplies policies; w9pt does not require a standalone process. The semantic core should remain isolated from both 9P wire details and S3 APIs so future protocol or storage adapters can reuse it.

### 6.2 Private bucket layout

One possible format:

```text
w9pt/<filesystem-id>/
  format                         # immutable format/version descriptor
  refs/current                   # small fenced writer/root record
  meta/manifests/<generation>    # durable metadata manifests
  meta/tables/<hash>             # immutable LSM tables or B-tree pages
  data/segments/<shard>/<id>     # immutable packed extent frames
  snapshots/<name>               # optional retained roots
```

Visible filenames must not be S3 object keys. Users should not mutate this prefix directly with S3 APIs. Provide separate import/export tools if direct object interoperability is required.

### 6.3 Inode model

Each inode needs at least:

```text
inode_id: stable 64-bit ID, never reused
kind: regular | directory | symlink | optional special types
mode, uid, gid
atime, mtime, ctime, optional birth time
size, allocated size
link_count
generation/data_version
extent_map_root (regular file)
directory_root (directory)
symlink_target (symlink)
xattr_root (if supported)
```

Map `inode_id` to `qid.path`, and increment/cache-map `generation` into `qid.version`/9P2000.L data-version fields. Directory entries map `(parent inode, name)` to inode IDs. This separation enables rename and hard links without moving file data.

### 6.4 Data plane

For mutable random-access files, prefer fixed logical extents rather than SteamPipe-style content-defined chunks:

- A `Twrite(offset, data)` touches known extent indexes.
- Partial extent writes perform read-modify-write in cache.
- Full extent writes need no old data.
- Truncate drops extent pointers beyond the new size.
- Sparse holes are missing/zero extent pointers.
- New extent versions are immutable.

Do not store every 32–256 KiB extent as an individual S3 object; request cost and latency would dominate. Pack many frames into 64–256 MiB immutable segment objects and store byte-range pointers in metadata. Use ranged GETs and coalesce adjacent frames.

### 6.5 Metadata and transaction plane

Namespace operations need atomic metadata transactions:

- `create`: allocate inode and add directory entry together.
- `unlink`: remove directory entry, decrement link count, and create an orphan record if still open.
- `link`: add another directory entry and increment link count.
- `renameat`: validate source/destination and update both directories plus link counts as one commit.
- `setattr`: apply all requested attribute changes or none.
- `write/truncate`: update extent map, size, times, and generation together.

For one server, serialize commits through one transaction coordinator. An object-backed LSM tree can batch mutations and periodically publish a manifest. A global root compare-and-swap can make cross-directory changes atomic, but it becomes a contention point. Sharding metadata improves throughput but makes cross-shard rename and link transactions harder.

### 6.6 Write and `fsync` semantics

Recommended sequence:

```text
Twrite
  -> validate fid/open mode/lock/permissions
  -> update dirty extent in RAM + local durable journal/cache policy
  -> update in-memory inode generation/size
  -> Rwrite

Tfsync
  -> seal dirty frames into segment(s)
  -> upload segment(s) and wait for successful S3 completion
  -> flush/upload metadata tables
  -> publish fenced metadata manifest/root
  -> fsync local journal/checkpoint as needed
  -> Rfsync
```

The durable manifest must never reference a segment that was not successfully uploaded. `Rfsync` must not be sent until both file data and all required metadata/namespace changes are durably reachable from the committed root.

Un-fsynced writes may be lost on server crash, as with ordinary buffered filesystems, but the server must never return success for `fsync` and later recover an earlier state. Delayed errors must be surfaced through `fsync` or `clunk`/close behavior as the client permits.

### 6.7 Open-unlink behavior

When a directory entry is unlinked while a fid remains open:

1. Remove the name atomically and decrement link count.
2. Keep the inode and extents reachable through live fid state.
3. Persist an orphan record if crash recovery must retain the open inode.
4. Reclaim it after the final fid clunks and link count is zero.

This cannot be implemented correctly by deleting a direct path-keyed S3 object immediately.

### 6.8 Locking

9P2000.L provides advisory byte-range lock operations, but S3 provides no lock manager. A single server can keep locks in memory and associate them with client/session/open state. For restart or HA:

- either declare locks lost and force remount/recovery;
- replicate live lock state to a standby;
- or implement leased locks with fencing tokens in a strongly coordinated metadata service.

S3 objects plus wall-clock expiry are not sufficient by themselves for safe distributed fencing. Conditional writes help elect/fence a writer, but lease renewal, pause, network partition, and stale-writer behavior need a formal protocol and fault testing.

### 6.9 Caching

At least three server caches are needed:

- decoded inode/directory/metadata pages;
- encrypted/compressed S3 segment byte ranges on disk;
- decoded/decompressed hot extents in memory.

Negative lookup caching and directory enumeration snapshots also matter. Every uncached `walk`/`getattr` becoming an S3 `HEAD` would make ordinary shell operations slow and expensive. AWS's Mountpoint documentation similarly notes that every path component lookup may incur S3 requests.

Start Linux clients with `cache=none` for coherence tests. Later, support `cache=mmap` only after qid/data-version invalidation, writeback, `fsync`, and multi-client visibility are verified.

### 6.10 Garbage collection

Immutable segments accumulate obsolete extent frames. GC must:

1. determine live extent pointers from the current metadata state and retained snapshots;
2. delete completely dead segments;
3. copy live frames out of fragmented segments;
4. atomically update pointers before deleting old segments;
5. tolerate crashes at every step;
6. avoid racing current readers, snapshots, or an old fenced writer.

S3 lifecycle rules alone cannot understand filesystem reachability.

## 7. Operation feasibility matrix

| Operation | Feasible? | Required implementation |
| --- | --- | --- |
| Lookup/walk | Yes | Directory metadata index and stable qids |
| Readdir | Yes | Ordered/snapshot directory iterator and stable cookies/offsets |
| Create/mkdir | Yes | Transactional inode allocation + directory entry |
| Read/pread | Yes | Extent lookup, range GET, cache, decode |
| Write/pwrite | Yes | Dirty extent cache, RMW, immutable segment append |
| Append | Yes | Server serializes EOF allocation; do not rely on S3 append |
| Truncate | Yes | Extent-map update and tombstones |
| Unlink open file | Yes | Live fid refs + orphan lifecycle |
| Atomic file rename | Yes | Metadata transaction; no data copy |
| Atomic directory rename | Yes | Metadata transaction; scalable subtree representation |
| Rename replace | Yes | Atomic source/destination/link-count transaction |
| Hard link | Yes | Shared inode ID and transactional link count |
| Symlink/readlink | Yes | Symlink inode payload |
| chmod/chown/times | Yes | Mutable inode metadata and authorization |
| `fsync` | Yes | Ordered segment + metadata manifest durability barrier |
| Advisory byte-range locks | Yes | Server lock manager; HA semantics must be defined |
| xattrs | Yes in protocol | Separate metadata records; size/namespace policy needed |
| POSIX ACLs | Possible but awkward | xattr/ACL model plus identity mapping; client compatibility |
| Sparse files | Yes internally | Missing extents; stock client feature exposure varies |
| `mmap` | Possible | Correct client cache/writeback/invalidation behavior |
| Device nodes/FIFOs/sockets | Optional/high risk | Metadata type emulation; often reject with `EOPNOTSUPP` |
| Inotify across clients | Not fully | 9P has no general remote invalidation/watch protocol |
| All `renameat2` flags | No via stock operations | Private extension or explicit unsupported errors |
| Reflink/copy-on-write API | Not stock 9P | Private extension; internal extent sharing is possible |
| Transparent reconnect | Not stock 9P | Custom dialect/client or remount |

## 8. Concurrency and high availability

### 8.1 Single authoritative server

This is the recommended first target. One process owns:

- mutation ordering;
- dirty data;
- fids and open-unlinked files;
- advisory locks;
- metadata commit sequencing;
- garbage collection.

S3 supplies durable storage, not distributed coordination. A server restart loses live session state, but the durable filesystem can recover consistently from its last committed manifest.

### 8.2 Multiple active gateways

Active/active servers greatly expand scope:

- distributed inode/directory transactions;
- lock and open-state coordination;
- cache invalidation;
- writer fencing;
- duplicate/retry handling after connection loss;
- safe GC while other nodes retain old roots;
- session recovery or client-visible failures.

A single CAS root can serialize every mutation correctly but may cap throughput and requires conflict retries. Partitioned roots scale better but need distributed transactions for rename/link across partitions. A transactional metadata service is usually cleaner than rebuilding consensus from S3 calls.

### 8.3 Active/standby

Active/standby is a more realistic second milestone:

- only one writer epoch may publish;
- standby follows metadata manifests and optionally receives dirty/live state;
- failover must fence the old writer before accepting mutations;
- stock v9fs clients still cannot restore fids automatically, so they remount unless a custom client/dialect is added.

## 9. Security model

### 9.1 9P side

- Do not expose unauthenticated plaintext 9P TCP directly to untrusted networks.
- Prefer Unix sockets, virtio/vsock, or loopback for local/VM use.
- Across hosts, use WireGuard/VPN, SSH tunneling, or an authenticated TLS proxy; stock v9fs does not natively negotiate general TLS.
- Bind each attach/fid tree to an authenticated identity and export root.
- Do not trust client-supplied numeric UIDs without host trust or an authenticated mapping.
- Enforce permissions server-side on every operation, not only in the client.
- Strictly bound `msize`, string lengths, element counts, fids, tags, queued writes, and lock records.

### 9.2 S3 side

- Give the server a least-privilege IAM role limited to one private prefix/bucket.
- Block direct human/application writes to the internal filesystem prefix.
- Require TLS and verify endpoints for S3-compatible providers.
- Use S3 checksums plus internal authenticated hashes/checksums for every frame/segment.
- Encrypt before upload when provider-side encryption is not enough for the threat model.
- Keep format metadata and wrapped keys versioned and recoverable.
- Use conditional writes for immutable-object creation and writer fencing.
- Enable bucket versioning carefully: it aids recovery but can accumulate old roots/delete markers and cost.

## 10. AWS-provided comparison points

### 10.1 Mountpoint for Amazon S3

Mountpoint is an Apache-2.0 FUSE implementation of the transparent adapter model. It is optimized for large reads and sequential writes and intentionally rejects semantics S3 cannot efficiently provide. Its [semantics document](https://github.com/awslabs/mountpoint-s3/blob/main/doc/SEMANTICS.md) is an excellent negative requirements list for a direct path-key design.

Use it instead of building 9P when applications only need its supported object-like operations. Do not use it as evidence that S3 natively supplies POSIX semantics.

### 10.2 S3 Files

AWS now offers [S3 Files](https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-files.html), a managed shared filesystem linked to an S3 bucket. It is built using Amazon EFS infrastructure, serves NFSv4.1/v4.2, maintains a high-performance file layer, and synchronizes changes with S3. This validates the need for an actual filesystem layer in front of S3.

It is not literally feature-complete POSIX: AWS documents no hard links, ACLs, Kerberos, pNFS, delegations, several special files, named attributes, and many optional NFSv4.2 operations. S3 export can lag file writes after an inactivity window, and underlying object rename/sync still has S3 limitations. See [S3 Files limitations](https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-files-quotas.html) and [synchronization behavior](https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-files-synchronization.html).

If the deployment is AWS-only, NFS clients are acceptable, and a managed service fits the cost/feature envelope, S3 Files should be evaluated before developing a new 9P filesystem.

## 11. Kubernetes CSI and StorageClass support

ZeroFS currently ships a first-party CSI driver under [`zerofs/zerofs-csi`](https://github.com/Barre/ZeroFS/tree/main/zerofs/zerofs-csi). It registers as `csi.zerofs.net` and includes deployable Kubernetes manifests plus an example `StorageClass`. The implementation was completed upstream in June 2026 and is present in the current v2.3.2 source line checked on 2026-09-01.

### 11.1 Architecture

One ZeroFS **gateway pair** backs one StorageClass:

```text
S3 bucket/prefix
        │
        ▼
ZeroFS gateway pair
  leader StatefulSet + standby StatefulSet
  one shared filesystem and one active writer
        │
        ├── admin RPC :7000 ← CSI controller
        ├── 9P :5564       ← CSI node plugins
        └── replication :9000 between gateway nodes

StorageClass zerofs
  PVC A -> /volumes/pvc-a
  PVC B -> /volumes/pvc-b
  PVC C -> /volumes/pvc-c
```

The driver does not start a separate ZeroFS instance or create a separate object prefix for every PVC. Each dynamically provisioned volume is a directory under `volumesRoot` (default `/volumes`) inside the shared gateway filesystem.

- `CreateVolume` calls the gateway admin RPC to create `/volumes/<volume-id>`.
- `DeleteVolume` renames the directory into trash and returns; deletion/extent GC continues in the background.
- `NodePublishVolume` spawns `zerofs mount <gateway-a>,<gateway-b> <target> --aname /volumes/<volume-id> --access all`.
- Each pod sees a FUSE mount backed by the private reconnectable 9P client.
- The mount probes both gateway addresses and follows the serving leader after failover.
- The attach root prevents path walking above the volume directory in normal client behavior, but it is namespacing rather than authentication.

The controller is a Deployment with Kubernetes's external-provisioner sidecar. The node service is a privileged DaemonSet with `/dev/fuse`, bidirectional mount propagation under `/var/lib/kubelet`, the CSI registrar, and liveness sidecar. Deploy manifests are in [`zerofs-csi/deploy`](https://github.com/Barre/ZeroFS/tree/main/zerofs/zerofs-csi/deploy).

### 11.2 StorageClass

The provided example is structurally:

```yaml
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: zerofs
provisioner: csi.zerofs.net
parameters:
  adminEndpoint: >-
    http://zerofs-gateway-a.zerofs.svc:7000,
    http://zerofs-gateway-b.zerofs.svc:7000
  gateway: >-
    zerofs-gateway-a.zerofs.svc:5564,
    zerofs-gateway-b.zerofs.svc:5564
  volumesRoot: /volumes
  csi.storage.k8s.io/provisioner-secret-name: zerofs-csi-admin
  csi.storage.k8s.io/provisioner-secret-namespace: zerofs
reclaimPolicy: Delete
volumeBindingMode: Immediate
allowVolumeExpansion: false
```

`adminEndpoint` and `gateway` are required and each contains both HA node addresses. `DeleteVolume` does not receive StorageClass parameters from CSI, so the example repeats the admin endpoint in a referenced provisioner Secret. One cluster can run several StorageClasses, each pointing to a different gateway pair, object-store path, and encryption password.

See the [example StorageClass manifest](https://github.com/Barre/ZeroFS/blob/main/zerofs/zerofs-csi/deploy/storageclass-example.yaml) and [CSI guide](https://github.com/Barre/ZeroFS/blob/main/documentation/src/app/kubernetes-csi/page.mdx).

### 11.3 Supported capabilities

| Kubernetes/CSI capability | Current status |
| --- | --- |
| Dynamic provisioning | Yes |
| Persistent lifecycle mode | Yes |
| `ReadWriteOnce` | Yes |
| `ReadOnlyMany` | Yes |
| `ReadWriteMany` | Yes |
| `ReadWriteOncePod` | No |
| Mount volumes | Yes |
| Raw `volumeMode: Block` | No |
| Volume expansion | No |
| Per-volume snapshots/clones | No |
| Per-volume capacity enforcement | No |
| StorageClass mount option passthrough | No; options are rejected |
| Controller attach step | None; `attachRequired: false` |
| Gateway leader failover | Yes, with reconnectable 9P mounts |

The requested PVC capacity is recorded in the PV but not enforced. All PVCs consume the same filesystem quota; `max_size_gb` caps the gateway filesystem as a whole. `NodeGetVolumeStats` reports whole-filesystem numbers rather than a per-PVC quota.

### 11.4 Operational limitations

- No first-party Helm chart was found in the current repository; installation is documented with raw `kubectl apply` manifests.
- The gateway is one leader plus one standby, not active/active scale-out. All StorageClass volumes share its metadata/data path and performance envelope.
- Node mounts are child `zerofs mount` processes of the CSI node container. Restarting that container breaks published mounts on the node until affected workload pods are recreated. The DaemonSet uses `OnDelete` updates so operators can drain and upgrade nodes deliberately.
- Every node needs `/dev/fuse`, a privileged CSI node pod, and bidirectional mount propagation.
- The gateway example itself requests local cache PVCs. Those need an existing/default underlying Kubernetes StorageClass or an explicitly configured local/cache storage provisioner; S3 remains the durable source of truth, but the cache volumes are operational dependencies.
- No per-volume snapshots exist. ZeroFS checkpoints cover the complete shared gateway filesystem.
- An open-unlinked descriptor becomes stale if the mount reconnects during gateway failover; other linked opens normally recover by inode ID.
- `reclaimPolicy: Delete` removes the volume directory asynchronously. Use `Retain` if automatic data deletion is unacceptable, after verifying the operational recovery workflow.

### 11.5 Security limitations

The three gateway ports are unauthenticated:

- 9P `5564`: any reachable client can choose an attach path, including another PVC or the filesystem root.
- Admin RPC `7000`: root-equivalent and able to create/trash volume directories.
- Replication `9000`: controls leader/standby replication and role discovery.

The shipped [NetworkPolicy example](https://github.com/Barre/ZeroFS/blob/main/zerofs/zerofs-csi/deploy/networkpolicy-example.yaml) restricts 9P to CSI node pods, admin RPC to the controller, and replication to the gateway pods. It provides no protection when the cluster CNI does not enforce NetworkPolicy, including common default flannel setups. The gateway must not share an unrestricted network with untrusted pods.

### 11.6 Test status

The project reports:

- `csi-sanity` v5.4.0: 31 declared-capability specs pass; one capacity-mismatch spec is skipped because per-volume capacity is not stored/enforced.
- A k3s end-to-end workflow applies the checked-in manifests, dynamically provisions an RWX PVC, shares it across two pods, verifies cross-mount reads/writes, deletes the active gateway leader, verifies mount continuation through standby promotion, and checks PV reclamation.

These are upstream project tests, not an independent production certification. The combination of unenforced capacity, privileged FUSE node pods, mount loss on node-plugin restart, unauthenticated control/data ports, and missing snapshots should be treated as v1 operational constraints.

### 11.7 Verdict

Yes, ZeroFS can currently back a Kubernetes StorageClass and dynamically provision RWX PVCs. It is suitable for controlled evaluation and workloads that accept the documented v1 constraints. Before production use, validate failover, node-plugin upgrades, S3 outage behavior, cache sizing, CNI NetworkPolicy enforcement, backup/checkpoint recovery, and aggregate gateway saturation with the intended workload.

## 12. Build-versus-adopt decision

| Choice | Advantages | Risks/limits |
| --- | --- | --- |
| Use Mountpoint S3 | Mature open thin adapter, high S3 throughput | Explicitly not full FS; FUSE, not 9P |
| Use AWS S3 Files | Managed filesystem semantics and NFS | AWS-only, managed cost/model, documented missing features |
| Use ZeroFS as reference/benchmark | Exact 9P+S3 runtime exists with substantial tests | Not w9pt's library architecture; AGPL/commercial and format coupling prevent direct adoption |
| Implement w9pt framework | Library-first API and backend control | Large correctness surface if production/HA/full POSIX is expected |
| Build on external metadata DB + S3 | Easiest distributed transactions/HA | Additional service; not S3-only |

The first engineering task should define w9pt's public protocol/session, semantic-core, and backend contracts. ZeroFS should be run separately as a reference benchmark for extent packing, metadata behavior, recovery, and test strategy—not adopted as w9pt's process model or embedded API.

## 13. Recommended phased plan

### Phase 0: define “full”

Create a required/optional/unsupported operation matrix covering:

- file/dir/symlink/hardlink operations;
- random I/O, append, truncate, sparse behavior, `mmap`;
- ownership, mode, timestamps, xattrs, ACLs;
- `fsync` and crash guarantees;
- locks and multi-client visibility;
- reconnect and server failover;
- special files, `ioctl`, notifications, advanced rename/fallocate/copy.

### Phase 1: reference validation

- Deploy ZeroFS locally against MinIO and against the target S3 service.
- Mount stock Linux v9fs with `cache=none`.
- Run representative workloads plus pjdfstest/xfstests subsets.
- Measure metadata latency, random I/O, sequential throughput, request count, S3 cost, cache hit rate, and `fsync` latency.
- Kill the server/S3 connection at every commit stage and verify recovery.

### Phase 2: w9pt semantic core prototype

- Implement stable inodes, directories, attrs, symlinks, hardlinks, and transactions independent of 9P.
- Use a local metadata DB and local segment store first.
- Add fixed extents, packed segments, range reads, truncation, orphan handling, and GC.
- Validate with a direct test API before adding a network protocol.

### Phase 3: embeddable 9P2000.L session framework

- Use an existing protocol library where possible: [hugelgupf/p9](https://github.com/hugelgupf/p9) is an active Apache-2.0 Go client/server library; Rust options include [`rs9p`](https://docs.rs/rs9p/latest/rs9p/). Do not import ZeroFS protocol crates into the Apache-2.0 implementation without separately compatible terms.
- Map qids/fids to stable inode/open state.
- Implement protocol resource limits, cancellation, errors, identities, and `fsync` precisely.
- Start with local Unix/TCP and no client caching.

### Phase 4: S3 backend

- Make segments and metadata objects immutable.
- Add local disk cache/WAL and conditional publish/fencing.
- Guarantee data-before-metadata ordering.
- Add object-backed metadata manifests/LSM or choose an external transactional store.
- Implement crash-safe compaction and GC.

### Phase 5: concurrency and operations

- Add multi-client coherence tests and `cache=mmap` if required.
- Add authentication/transport protection.
- Add active/standby only after writer fencing and recovery tests pass.
- Consider a private reconnect/compound extension only if stock-remount behavior is inadequate.

## 14. Failure tests required before claiming filesystem semantics

- Server dies before/after segment upload, metadata upload, and root publication.
- S3 returns timeout after executing a conditional write.
- Two writers race to update the root/fencing record.
- Partial extent write races truncate or another write.
- Rename replaces an existing destination while both files are open.
- File is unlinked with multiple live fids and then server crashes.
- Lock holder disconnects, pauses, reconnects, or is fenced.
- Directory enumeration races create/unlink/rename.
- GC races a reader pinned to an old extent and a retained snapshot.
- Local cache contains corrupt/truncated data.
- S3 object exists but checksum, encryption tag, or metadata pointer is corrupt.
- Credentials expire during multipart upload or `fsync`.
- Bucket versioning/delete markers change expected conditional-write behavior.
- Client retries a mutation after the reply is lost.
- Stock v9fs connection drops with dirty writeback pages.
- Two clients use `cache=loose` and observe conflicting stale data.
- Metadata space, configured quota, local cache, or S3 multipart limits are exhausted.

## 15. Final feasibility verdict

### Can 9P and S3 be paired?

Yes. 9P is a reasonable front-end protocol for a Linux-focused S3-backed filesystem, especially for a first implementation.

### Can a direct 9P-to-S3 key adapter provide full operations?

No. It can provide an object-shaped filesystem subset only.

### Can an S3-only durable filesystem engine provide broad POSIX operations over 9P?

Yes, by storing an opaque filesystem format—immutable packed extents plus transactional metadata—in S3 and letting an embedding application run authoritative w9pt session/core objects. ZeroFS is proof of storage-engine feasibility, not the required process model.

### Can it provide literally every local Linux filesystem feature?

No, not through stock 9P2000.L. The project must define supported semantics and return explicit errors for the rest.

### Recommended next decision

Define the embeddable API contracts, then prototype w9pt's inode/extent/transaction core locally before connecting it to S3 or 9P transports. Use ZeroFS only as an external behavioral reference. The storage semantics are the hard part; the 9P codec is not.

## 16. Primary and implementation references

9P and Linux:

- [Plan 9: introduction to 9P](https://9p.io/magic/man2html/5/0intro)
- [Plan 9 system paper: 9P and fids](https://9p.io/sys/doc/9.html)
- [Plan 9: walk](https://9p.io/magic/man2html/5/walk)
- [Plan 9: stat/qid](https://9p.io/magic/man2html/5/stat)
- [Plan 9: read/write](https://9p.io/magic/man2html/5/read)
- [Linux kernel v9fs documentation](https://www.kernel.org/doc/html/latest/filesystems/9p.html)
- [diod 9P2000.L protocol description](https://github.com/chaos/diod/blob/master/protocol.md)
- [hugelgupf/p9 Go 9P2000.L library](https://github.com/hugelgupf/p9)
- [ZeroFS private 9P reconnect/idempotency extensions](https://github.com/Barre/ZeroFS/blob/main/documentation/src/app/9p-extensions/page.mdx)
- [ZeroFS per-connection 9P session handler](https://github.com/Barre/ZeroFS/blob/main/zerofs/src/ninep/handler.rs)
- [v86 9P WebSocket proxy framing](https://github.com/copy/v86/blob/master/docs/filesystem.md)
- [Wanix 9P WebSocket bridge](https://github.com/tractordev/wanix/blob/main/misc/ws9p/ws9p.go)
- [RFC 6455 — WebSocket](https://www.rfc-editor.org/rfc/rfc6455.html)
- [RFC 8441 — WebSocket over HTTP/2](https://www.rfc-editor.org/rfc/rfc8441.html)
- [RFC 9220 — WebSocket over HTTP/3](https://www.rfc-editor.org/rfc/rfc9220.html)

S3:

- [Amazon S3 consistency model](https://docs.aws.amazon.com/AmazonS3/latest/userguide/Welcome.html)
- [Amazon S3 object key and flat namespace model](https://docs.aws.amazon.com/AmazonS3/latest/userguide/object-keys.html)
- [Amazon S3 conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html)
- [Amazon S3 copy/move/rename behavior](https://docs.aws.amazon.com/AmazonS3/latest/userguide/copy-object.html)
- [Amazon S3 object metadata behavior](https://docs.aws.amazon.com/AmazonS3/latest/userguide/UsingMetadata.html)
- [Amazon S3 multipart limits](https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html)
- [S3 directory-bucket RenameObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_RenameObject.html)
- [S3 directory-bucket append](https://docs.aws.amazon.com/AmazonS3/latest/userguide/directory-buckets-objects-append.html)

Reference systems:

- [ZeroFS repository and architecture overview](https://github.com/Barre/ZeroFS)
- [ZeroFS Kubernetes CSI guide](https://github.com/Barre/ZeroFS/blob/main/documentation/src/app/kubernetes-csi/page.mdx)
- [ZeroFS CSI driver source and deployment manifests](https://github.com/Barre/ZeroFS/tree/main/zerofs/zerofs-csi)
- [ZeroFS example StorageClass](https://github.com/Barre/ZeroFS/blob/main/zerofs/zerofs-csi/deploy/storageclass-example.yaml)
- [Mountpoint for Amazon S3](https://github.com/awslabs/mountpoint-s3)
- [Mountpoint filesystem semantics](https://github.com/awslabs/mountpoint-s3/blob/main/doc/SEMANTICS.md)
- [AWS S3 Files](https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-files.html)
- [AWS S3 Files limitations](https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-files-quotas.html)

Licensing and clean-room background:

- [ZeroFS AGPL-3.0 license file](https://github.com/Barre/ZeroFS/blob/main/LICENSE)
- [GNU AGPL version 3](https://www.gnu.org/licenses/agpl-3.0.en.html)
- [GNU license FAQ](https://www.gnu.org/licenses/gpl-faq.en.html)
- [U.S. Copyright Office: Computer Programs](https://www.copyright.gov/register/tx-programs.html)
- [U.S. Copyright Office report discussing software methods and clean-room implementation](https://www.copyright.gov/policy/software/software-full-report.pdf)
