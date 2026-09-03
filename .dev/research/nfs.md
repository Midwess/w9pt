# How NFS works

Research status: initial survey  
Last checked: 2026-09-01  
Project context: w9pt is an embeddable 9P framework for pluggable storage backends; its first backend is S3-compatible object storage.

## Executive summary

The Network File System (NFS) is a remote **file** protocol. It lets a client operating system present a server-owned namespace through its normal filesystem API. It is not a remote block device and it does not send POSIX system calls verbatim. The client translates local operations into ONC RPC requests; the server resolves opaque file handles, authenticates and authorizes each request, performs an equivalent backend operation, and returns data, attributes, or an NFS error.

The ideas that matter most are:

- Objects are addressed primarily by opaque, server-issued **file handles**, not full paths. Path traversal is a sequence of directory lookups that produces more handles.
- ONC RPC supplies request/reply framing, authentication fields, request identifiers, and transport independence. XDR supplies the canonical wire encoding.
- Client-side data, attribute, and directory-entry caches are central to performance. Normal NFS offers weaker coherence than a tightly coupled cluster filesystem; close-to-open consistency is the common baseline.
- NFSv3's core protocol is designed to be stateless. Mounting, locking, and crash notification use separate RPC programs. NFSv4 is stateful: opens, byte-range locks, delegations, leases, and recovery are in the main protocol.
- NFSv4 uses `COMPOUND` to batch dependent operations into one RPC. A compound is ordered but is **not** an atomic transaction.
- NFSv4.1 sessions add replay detection, bounded reply caching, exactly-once request execution within the session rules, bidirectional channels, connection trunking, and pNFS.
- NFSv4.2 is the latest published minor version as of this survey. Its facilities are optional extensions to v4.1, including server-side copy/clone, sparse-file operations, space reservation, and I/O hints. “Supports 4.2” does not mean “supports every 4.2 operation.”
- A successful `WRITE` does not always mean the bytes are on durable media. The request and reply describe stability; `COMMIT`/`fsync` is the durability boundary when unstable writes are used.
- Traditional `AUTH_SYS` trusts client-supplied numeric user/group IDs and should be confined to trusted environments. RPCSEC_GSS/Kerberos and RPC-with-TLS address different parts of the security problem.
- NFS is not w9pt's initial protocol target. A future NFS adapter could reuse w9pt's semantic core only after stable identities, random-access I/O, atomic namespace operations, durable commit behavior, cache validation, authorization, locking/state, and crash recovery are defined independently of 9P.

## 1. Mental model

The common data path is:

```text
application
    │ open/read/write/stat/rename/fsync
    ▼
client VFS + NFS client
    │ cache lookup, RPC construction, XDR encoding
    ▼
TCP (normally) / optional RDMA / legacy UDP
    ▼
server RPC transport
    │ authenticate, decode, replay check, dispatch
    ▼
NFS namespace + state manager
    │ resolve file handle, authorize, execute operation
    ▼
server filesystem or storage adapter
```

The NFS client is a filesystem implementation inside or alongside the client kernel. The application normally does not know that a path is remote. The client absorbs much of the semantic translation: it caches pages and metadata, breaks large reads/writes into protocol-sized requests, retries requests, turns server errors into local errors, and maintains open/lock state for NFSv4.

The server exports selected filesystem trees. It must treat all client data as untrusted, resolve handles without relying on a current pathname, check the request's credentials and export policy, serialize conflicting operations, and make accurate claims about durability.

### NFS is not these things

- **Not a block protocol:** clients request files, directories, attributes, and byte ranges, not sectors. iSCSI and NVMe-oF are block protocols.
- **Not object storage:** NFS provides a hierarchical mutable namespace and filesystem operations such as lookup, rename, links, and byte-range writes. Object stores usually expose key/object operations with different atomicity and identity rules.
- **Not a distributed consensus protocol:** it does not make unrelated application updates transactional or coordinate arbitrary multi-file invariants.
- **Not automatically POSIX-perfect:** a client presents a POSIX-like API, but caching, failures, heterogeneous servers, and protocol differences create observable gaps.

## 2. Protocol stack and wire format

### 2.1 XDR

NFS data structures are described and serialized with External Data Representation (XDR). XDR defines a machine-independent, big-endian representation for integers, arrays, opaque byte sequences, strings, structures, and discriminated unions. Most variable-length fields carry a length and are padded to four-byte boundaries. The current base specification is [RFC 4506](https://www.rfc-editor.org/rfc/rfc4506.html).

XDR is only encoding. It does not supply transport, retries, authorization, or filesystem behavior.

### 2.2 ONC RPC

NFS is an ONC Remote Procedure Call application. A call identifies an RPC program, program version, and procedure, and carries:

- a 32-bit transaction ID (`xid`) used to associate a reply with a call;
- call/reply discriminants and acceptance/rejection status;
- an authentication credential and verifier;
- XDR-encoded procedure arguments or results.

ONC RPC does not guarantee timeout, retry, duplicate suppression, or execution semantics on its own. Those responsibilities belong to the application protocol and implementation, especially over UDP. Over a byte stream such as TCP, ONC RPC uses record marking to delimit messages. See [RFC 5531](https://www.rfc-editor.org/rfc/rfc5531.html).

### 2.3 Programs, procedures, and operations

NFSv2/v3 expose one RPC procedure per filesystem action: `LOOKUP`, `GETATTR`, `READ`, `WRITE`, `CREATE`, `REMOVE`, `RENAME`, `READDIR`, and so on. Supporting services such as MOUNT and NLM are separate RPC programs.

NFSv4's main RPC program has only `NULL` and `COMPOUND` procedures. A `COMPOUND` carries an ordered list of NFS operations such as `PUTROOTFH`, `LOOKUP`, `GETFH`, `OPEN`, `GETATTR`, or `READ`. Each operation can use the current file handle established by an earlier operation. Processing stops at the first failing operation.

For example, this conceptual v4 compound resolves `/projects/w9pt` and asks for its handle and attributes:

```text
COMPOUND {
  PUTROOTFH
  LOOKUP "projects"
  LOOKUP "w9pt"
  GETFH
  GETATTR { type, size, fileid, fsid, change, mode, owner, owner_group }
}
```

This saves round trips, but it is not a transaction. Earlier operations may have taken effect when a later one fails. [RFC 7530](https://www.rfc-editor.org/rfc/rfc7530.html) defines NFSv4.0's compound model; [RFC 8881](https://www.rfc-editor.org/rfc/rfc8881.html) defines v4.1.

## 3. Filesystem object model

### 3.1 Namespace and object types

The exported namespace is hierarchical. Regular files are opaque byte streams. The protocols also model directories, symbolic links, and, depending on version and server capability, hard links and special files. Objects expose protocol attributes such as:

- object type and size;
- filesystem and file identifiers;
- mode/permission information;
- owner and group;
- access, modification, and metadata-change times;
- link count and space used;
- supported operations and filesystem limits;
- a v4 `change` attribute used for cache validation;
- optional ACL, named-attribute, layout, sparse-file, and other version-specific metadata.

Not every server backend can support every operation or attribute. Protocol capability discovery and precise `NOTSUPP`-style errors are therefore part of interoperability, not an edge case.

### 3.2 Opaque file handles

A file handle is a server-generated identifier for a filesystem object. The client must not interpret its bytes. The handle normally encodes or indexes enough information for the server to recover an object independent of its current pathname; this is why a rename need not invalidate an open client's reference.

Important properties:

- A handle is meaningful only in the issuing server/filesystem context.
- Distinct paths through hard links may lead to the same object and handle.
- A client must be ready for `STALE` when an object was removed, an export changed, a volatile handle expired, or the server can no longer resolve it.
- NFSv3 handles are variable-length up to 64 bytes. NFSv4 allows persistent and explicitly volatile handles and permits a larger opaque value.
- A handle identifies an object; it is not sufficient authorization. The server must still validate credentials and export access on every operation.
- A server implementation needs a strategy against handle guessing or forgery. Linux filesystems implement an object-to-handle and handle-to-object contract; current Linux nfsd can optionally MAC-sign handles, as described in the kernel's [exportability documentation](https://www.kernel.org/doc/html/latest/filesystems/nfs/exporting.html).

Stable file handles are one of the hardest requirements for a synthetic or object-store-backed NFS server. A key derived only from a pathname breaks on rename. A process-local table breaks on restart. A reusable inode number without a generation counter can silently resolve an old handle to a new object.

### 3.3 Path traversal

NFS operations generally act on `(directory handle, component name)` or on an object handle, rather than accepting an arbitrary absolute path. Consequently:

1. The client starts with an export/root handle.
2. `LOOKUP` resolves one component within a directory.
3. The returned handle becomes the base for the next component.
4. The client caches positive and negative lookup results subject to validation rules.

This design supports rename-stable identity and avoids repeatedly sending full paths, but increases the importance of handle validity and directory-cache correctness.

### 3.4 Directory enumeration

`READDIR` returns entries plus opaque continuation cookies. A cookie is not necessarily an array index or byte offset; clients must return it unchanged. A cookie verifier lets a server detect that the directory changed so much that continuing from an old cookie is unsafe. NFSv3 `READDIRPLUS`, and analogous v4 behavior, can return handles and attributes with entries to avoid a lookup/getattr RPC for every name.

A server adapter needs stable-enough pagination semantics. Mapping a changing object-store listing directly onto integer offsets is unsafe: insertions, deletions, and backend pagination-token expiry can cause duplicates, omissions, or invalid continuation. The adapter needs a snapshot, a validated opaque backend cursor, or an explicit restart/error strategy.

## 4. End-to-end request flows

### 4.1 Mounting an NFSv3 export

A typical v3 mount has several services:

1. Resolve the server name and contact `rpcbind`/portmapper, normally on port 111, to discover RPC service endpoints.
2. Contact the MOUNT v3 service (`mountd`) and send the export pathname.
3. `mountd` checks host/export policy and returns the export's initial file handle plus allowed security flavors.
4. Contact the NFS v3 service, normally on port 2049, and use that handle for `GETATTR`, `LOOKUP`, and later requests.
5. Use separate NLM/NSM services if byte-range locking and reboot notification are needed.

This creates firewall and operational complexity because auxiliary service ports may be dynamically assigned. MOUNT is an access gate and handle-discovery protocol, but actual NFS requests still require authorization.

### 4.2 Mounting and traversing NFSv4

NFSv4 does not use a separate MOUNT protocol. The server exposes a logical root, often a synthetic **pseudo-filesystem**, that connects its exports into one browsable tree. A client normally contacts the NFS service directly on TCP port 2049, obtains the root handle with `PUTROOTFH`, walks components with `LOOKUP`, and uses `SECINFO` when it must discover an export's accepted security mechanisms.

NFSv4.0 may require a separate server-to-client callback connection for delegations. NFSv4.1 creates sessions and can carry callbacks on a client-established connection's backchannel, which works better through stateful firewalls.

### 4.3 Opening and reading a file

For v3 there is no protocol-level `OPEN`. A simplified flow is:

```text
LOOKUP(parent_fh, "file") -> file_fh + attributes
ACCESS(file_fh, requested_mask) -> allowed_mask
READ(file_fh, offset, count) -> data + eof + attributes
```

The server still checks access on operations. `ACCESS` helps the client predict what a local `open(2)` should report, but it is not a durable grant and cannot replace the check at `READ` time.

For v4, `OPEN` creates share/open state and returns a stateid. A client may then send `READ(file_fh, stateid, offset, count)` and eventually `CLOSE`. The server can grant a read delegation with the open, allowing more work to be served locally until the delegation is recalled.

The client typically reads ahead and caches pages, so one application `read` is not necessarily one NFS `READ`, and repeated reads may cause no network traffic.

### 4.4 Writing and committing data

The client usually buffers dirty pages and sends byte-range `WRITE` requests asynchronously. Both v3 and v4 distinguish stability levels:

- **UNSTABLE:** the server has accepted the data but may lose it on restart.
- **DATA_SYNC:** file data, and enough metadata to retrieve it, are stable.
- **FILE_SYNC:** data and all required file metadata are stable.

The server reports the stability actually achieved. If writes remain unstable, the client issues `COMMIT` for a range before it tells an application that an `fsync`-like durability boundary succeeded. A server returns a write verifier tied to its current boot/storage instance. If the verifier changes, the client knows previously acknowledged unstable data may have been lost and must resend it from its cache. The full v3 mechanism is specified in [RFC 1813](https://www.rfc-editor.org/rfc/rfc1813.html).

Consequences:

- `WRITE` success and durable persistence are different facts.
- A server must not claim `FILE_SYNC` before its backend's real durability barrier succeeds.
- An object-store adapter cannot honestly implement random writes merely by buffering indefinitely and acknowledging stable completion.
- A failed `close` or `fsync` can be the first place a delayed write error reaches the application.
- Export/server settings that acknowledge changes before stable storage improve throughput but create crash-loss risk. Linux documents this tradeoff for `async` versus `sync` exports in [exports(5)](https://man7.org/linux/man-pages/man5/exports.5.html).

### 4.5 Rename, create, and remove

Namespace-changing operations use parent directory handles and names. Each individual operation has defined atomicity at the server, but a series of RPC operations is not a multi-operation transaction.

The client must handle the ambiguous-result problem: a request can execute on the server and its reply can be lost. Blindly reissuing a non-idempotent request can produce a different effect or error. NFS implementations use transaction IDs, duplicate-request caches, operation-specific replay handling, exclusive-create verifiers, and—starting in v4.1—session slot/sequence rules to make retries safer.

Removing an open file exposes a classic local/remote semantic gap. Some clients preserve local unlink-while-open behavior by renaming the file to a hidden temporary `.nfs...` name and deleting it on final close (“silly rename”). Crashes and permissions can leave these files behind. It is an implementation accommodation, not a general NFS transaction facility.

## 5. Version map

| Version | Character | Important additions or limits | Operational shape |
| --- | --- | --- | --- |
| NFSv2 | Historical, stateless core | 32-bit-era sizes/offsets, fixed 32-byte handles, small transfers; original spec used UDP | NFS + MOUNT + lock/status side protocols |
| NFSv3 | Mature, stateless core | 64-bit sizes/offsets, up-to-64-byte handles, `ACCESS`, `READDIRPLUS`, `FSINFO`, weak cache consistency data, asynchronous write plus `COMMIT` | Common on trusted LANs and for simple interoperability; usually TCP today, but multiple auxiliary RPC services remain |
| NFSv4.0 | Stateful integrated protocol | `COMPOUND`, pseudo-root, integrated open/locking, leases, recovery, delegations, rich attributes/ACL model, stronger security framework | One main protocol on TCP 2049; callbacks can require server-initiated connectivity |
| NFSv4.1 | Stateful sessions | Sessions, exactly-once machinery, better callback backchannel, connection trunking, pNFS, improved multi-server namespace and migration | Preferred baseline when implementing modern v4 semantics; [RFC 8881](https://www.rfc-editor.org/rfc/rfc8881.html) obsoletes the original RFC 5661 text |
| NFSv4.2 | Extensible v4.1 superset | Optional server-side copy/clone, sparse-file seek/read, allocate/deallocate, I/O advice, application data blocks, labeled NFS; later standards add more optional features | Capability negotiation is essential; base behavior still comes from v4.1 |

References: [NFSv2 / RFC 1094](https://www.rfc-editor.org/rfc/rfc1094.html), [NFSv3 / RFC 1813](https://www.rfc-editor.org/rfc/rfc1813.html), [NFSv4.0 / RFC 7530](https://www.rfc-editor.org/rfc/rfc7530.html), [NFSv4.1 / RFC 8881](https://www.rfc-editor.org/rfc/rfc8881.html), and [NFSv4.2 / RFC 7862](https://www.rfc-editor.org/rfc/rfc7862.html).

As of 2026-09-01, the IETF NFSv4 working group is active and maintains v4.0, v4.1, and v4.2. The latest published minor version is still 4.2. The extension model in [RFC 8178](https://www.rfc-editor.org/rfc/rfc8178.html) permits standards-track additions and corrections to a current minor version. Published examples include NFSv4 extended attributes in [RFC 8276](https://www.rfc-editor.org/rfc/rfc8276.html) and newer open/delegation features in [RFC 9754](https://www.rfc-editor.org/rfc/rfc9754.html). Therefore, implementations of the same advertised minor version can implement different valid feature subsets. The [working-group status page](https://datatracker.ietf.org/wg/nfsv4/about/) is the place to check ongoing work; Internet-Drafts are provisional and are not used as requirements in this note.

## 6. Statelessness, state, replay, and recovery

### 6.1 What “stateless NFSv3” means

The v3 NFS server does not require a protocol `OPEN` before `READ` or `WRITE`. A request normally contains enough information—file handle, byte range, credentials, and operation arguments—to execute independently. After an NFS service restart, clients can continue using persistent handles without rebuilding open state.

This does **not** mean a production v3 stack has no state:

- transports have connections and in-flight calls;
- servers keep duplicate-request/reply caches;
- clients cache data, attributes, and names;
- `mountd` tracks or reports mounts operationally;
- NLM maintains locks and NSM coordinates reboot notifications;
- unstable writes exist only in volatile server storage until committed.

It means the core NFSv3 read/write namespace protocol avoids mandatory per-open server state.

### 6.2 Idempotence and duplicate requests

Network failure produces several possibilities: the server never saw a request; it is still executing; it executed and the response was lost; or the client received a response after it gave up. The client cannot infer which case occurred from a timeout.

Reads and attribute queries are naturally safe to repeat. Operations such as create, remove, rename, truncate, and some locking actions require replay protection or operation-specific recovery. A server's duplicate-request cache generally keys on RPC identity and request context and returns the prior response rather than executing a detected retransmission again. Cache eviction and server reboot limit what older versions can guarantee.

### 6.3 NFSv4 stateids, owners, and leases

NFSv4 associates state with a client identity. Opens, share reservations, byte-range locks, delegations, and pNFS layouts are represented by server-issued `stateid` values. Sequence identifiers distinguish new owner operations from replays. A server grants a time-limited lease over client state; continued valid activity renews it. If the client stops renewing, the server may revoke the state after the lease period.

State improves correctness and cache performance but creates a recovery protocol. The implementation must distinguish:

- a lost transport from a lost session;
- a restarted client from a reconnecting client instance;
- a restarted server from a temporarily unreachable one;
- expired/revoked state from state that can be reclaimed.

### 6.4 Server restart and grace period

After a v4 server loses volatile state, it enters a grace period. Existing clients re-establish previously held opens and locks using reclaim forms. The server rejects conflicting new state until recovery is safe. Clients learn of lost state through errors such as stale client/state IDs or `GRACE`, recreate identity/session state as required, reclaim what they can, and surface failures if recovery is impossible.

A durable server implementation must either persist the necessary state/reply information or implement the specified reboot and grace behavior honestly. Simply reconstructing file data while forgetting all locks and delegations can corrupt concurrent applications.

### 6.5 NFSv4.1 sessions and exactly-once execution

An NFSv4.1 session is a long-lived object independent of any single TCP connection. Each channel has a bounded slot table. A request begins with `SEQUENCE`, identifying the session, slot, and per-slot sequence number. The receiver can then distinguish:

- the next request for a slot, which it executes;
- a replay of the current request, for which it returns the cached result;
- a misordered or invalid sequence, which it rejects.

This supplies exactly-once execution semantics for session requests under the rules in [RFC 8881, section 2.10](https://www.rfc-editor.org/rfc/rfc8881.html#name-session). Parallelism comes from multiple slots, not from reusing one slot concurrently. Exactly-once behavior can survive a server restart only when the server agreed to a persistent session and atomically persisted the required slot/reply state; otherwise the protocol reports loss rather than pretending the outcome is known.

Sessions also decouple client state from physical connections, permit multiple connections per session (trunking), and provide a backchannel on client-created connections.

## 7. Caching and consistency

### 7.1 Why caching is unavoidable

Without caching, ordinary pathname traversal, `stat`, reads, and writes would require frequent network round trips. NFS clients therefore cache:

- file data/pages;
- dirty write data awaiting writeback or commit;
- file and directory attributes;
- positive name-to-handle lookups;
- negative lookups (“this name does not exist”);
- directory enumeration results;
- access decisions for a bounded time;
- v4 delegation-backed authority.

The caches improve latency and reduce server load, but multiple clients can temporarily disagree. NFS is normally designed around useful sharing semantics, not instantaneous global cache coherence.

### 7.2 Close-to-open consistency

The common model is **close-to-open**:

1. When a client opens a file, it revalidates cached attributes/data against the server.
2. Writes may be cached during the open.
3. On close, pending writes are pushed to the server.
4. A later open by another cooperating client revalidates and should observe the completed writer's changes.

This works well for sequential “writer closes, then reader opens” sharing. It does not promise immediate observation while both applications keep the file open, and it cannot serialize uncoordinated concurrent writers. Linux's behavior and tuning are described in [nfs(5), Data and Metadata Coherence](https://man7.org/linux/man-pages/man5/nfs.5.html#DATA_AND_METADATA_COHERENCE).

### 7.3 Attribute and name caches

Clients validate cached file data using attributes. NFSv3 weak cache consistency (WCC) returns compact pre-operation and post-operation attributes around modifying operations, helping a client detect interference without extra RPCs. It is deliberately “weak”: it is evidence for cache management, not a serializable consistency protocol.

NFSv4 adds a `change` attribute that changes when the corresponding object changes, avoiding dependence only on timestamp resolution. Directory `change` values help invalidate cached names and enumeration state.

Timeouts remain important. A long attribute or negative-name cache lifetime improves performance but delays visibility of changes made by other clients. Disabling attribute caching is expensive and still does not magically make all application behavior atomic.

### 7.4 Delegations

An NFSv4 server may grant an open delegation:

- a **read delegation** promises to notify the client before allowing a conflicting writer;
- a **write delegation** promises to notify the client before allowing another conflicting reader or writer.

The client can then service eligible opens, closes, locks, reads, writes, and metadata actions locally. On a conflict, the server recalls the delegation via callback; the client flushes relevant data/state and returns it. Delegations are optional performance grants, never something a client may assume it will receive.

The correctness burden is substantial: callback races, recall timeouts, client failure, lease expiry, and server restart all need defined handling. v4.1 sessions improve callback routing and sequencing.

### 7.5 Locking and application coordination

NFSv3 uses NLM for advisory byte-range locks and NSM for reboot recovery. NFSv4 integrates share reservations and byte-range locks into the NFS state model. Locks are primarily advisory: cooperating applications must acquire them.

Clients normally flush or invalidate relevant caches around lock acquisition/release so a lock can serve as an application serialization point. Locks do not turn a group of unrelated file operations into a transaction. Memory-mapped I/O and applications that keep files open while bypassing the agreed lock discipline still need special care.

### 7.6 Practical consistency classification

| Pattern | Expected behavior |
| --- | --- |
| One client, no external writers | Strong local illusion through the client cache, subject to delayed write errors and server failures |
| Writer closes, then another client opens | Close-to-open normally exposes the completed write |
| Two clients keep a file open and access it concurrently without locks | Stale reads and write races are possible |
| Multiple clients use correct byte-range locks | Coordinated regions can be serialized, within recovery and failure rules |
| Multi-file update requiring all-or-nothing visibility | Not provided by NFS; application-level protocol/database required |
| Server unreachable on a hard mount | Operations normally keep retrying and may appear hung |
| Server unreachable on a soft mount | Operations eventually fail; partial/ambiguous I/O can threaten data integrity |

Linux warns that `soft`/`softerr` timeouts can cause silent corruption in some cases; `hard` is the normal data-integrity choice. See [nfs(5)](https://man7.org/linux/man-pages/man5/nfs.5.html).

## 8. Security and identity

Security has four separate questions:

1. **Who is the client or user?** Authentication.
2. **May that principal access this export/object/operation?** Authorization.
3. **Can an intermediary alter a request undetected?** Integrity.
4. **Can an intermediary read file data or metadata?** Privacy/confidentiality.

An export allowlist or a privileged source port does not answer all four.

### 8.1 AUTH_SYS

Traditional `AUTH_SYS` puts numeric UID, primary GID, and a limited auxiliary-group list in the RPC credential. The server largely trusts the client host to have authenticated the user and supplied truthful numbers. This has major consequences:

- UID/GID assignments must be coordinated across hosts.
- A compromised or untrusted client can forge identities.
- Network address/hostname export rules authenticate a location weakly, not a person.
- Reserved source ports are only a trust convention and are not cryptographic proof.
- Traffic and identity are normally visible and modifiable on the network unless another protection layer is used.

Linux enables `root_squash` by default for exports, mapping remote UID/GID 0 to an anonymous identity. `no_root_squash` is high risk. Squashing limits remote root; it does not repair forged non-root identities. See [exports(5)](https://man7.org/linux/man-pages/man5/exports.5.html).

### 8.2 RPCSEC_GSS and Kerberos

[RPCSEC_GSS](https://www.rfc-editor.org/rfc/rfc2203.html) adds a GSS-API security context and supports authenticated RPC messages with selectable service:

- `krb5`: strong principal authentication;
- `krb5i`: authentication plus message integrity;
- `krb5p`: authentication, integrity, and encrypted RPC arguments/results.

Kerberos introduces operational dependencies—KDC, principals, keytabs, clock synchronization, name canonicalization, credential renewal—but removes the need to trust arbitrary client-provided UID numbers. The server still maps authenticated principals to its authorization/ownership model.

### 8.3 RPC-with-TLS

[RFC 9289](https://www.rfc-editor.org/rfc/rfc9289.html) adds TLS negotiation to ONC RPC transports. TLS can protect the connection against observation and modification, and mutual TLS can authenticate peers. It composes with RPC authentication; it does not by itself define per-user filesystem identity or ACL semantics.

The base mechanism is designed to interoperate opportunistically with peers that lack TLS, so administrators must explicitly require protected transport when downgrade is unacceptable. Current Linux exports expose `xprtsec` policies for no TLS, TLS without client certificate, or mutual TLS; see [exports(5)](https://man7.org/linux/man-pages/man5/exports.5.html).

### 8.4 NFSv4 identity and ACLs

NFSv4 represents owners/groups in an internationalized name form and clients/servers map these to local identities. Domain and id-mapping mistakes commonly appear as anonymous/`nobody` ownership or failed authorization. Identity mapping is separate from cryptographic authentication.

NFSv4 ACLs are richer than POSIX ACLs and are not perfectly isomorphic with them. A server backed by a filesystem, object store, or custom metadata model must define what it preserves, rejects, or translates; silent lossy translation is dangerous. NFSv3 ACL support found in some systems is a non-standard side protocol, not part of RFC 1813.

### 8.5 Security rules for an implementation

- Authorize every operation after resolving the handle; never treat possession of a handle as permission.
- Bind state, sessions, and reclaim rights to the correct authenticated client/principal.
- Prevent a handle from escaping its export, especially for subdirectory exports and after rename.
- Reject malformed XDR, excessive lengths/counts, invalid UTF-8 where required, integer overflow, and impossible operation sequences before backend execution.
- Bound request, reply, state, and replay-cache resources per client to resist exhaustion.
- Do not expose v3 auxiliary services or `AUTH_SYS` to untrusted networks by default.
- Make security-flavor negotiation resistant to downgrade and ensure pseudo-root traversal cannot bypass a stricter export policy.
- Avoid logging file contents, credentials, GSS tokens, TLS keys, or reusable handles at normal verbosity.

## 9. Transport, discovery, and network behavior

### 9.1 Ports and transports

- NFSv4 normally uses TCP port 2049 directly and requires TCP support. v4.1 callbacks can use the session backchannel.
- NFSv2/v3 can use UDP or TCP, but TCP is the normal modern choice. UDP needs careful retransmission, duplicate detection, and fragmentation avoidance.
- v3 deployments commonly need rpcbind on port 111 plus NFS, mountd, lockd/NLM, statd/NSM, and sometimes quota services. Fixed auxiliary ports simplify firewalling.
- RPC-over-RDMA is an optional high-performance transport defined by [RFC 8166](https://www.rfc-editor.org/rfc/rfc8166.html) and related specifications.

TCP preserves bytes, not RPC or NFS request boundaries. Implementations must parse ONC RPC record markers, allow partial reads/writes, cap fragment/message size before allocation, and tolerate multiple in-flight calls and out-of-order completion where allowed.

### 9.2 Timeouts, retries, and “hung” processes

With a hard mount, a client normally retries an unavailable server indefinitely. The application syscall can block until connectivity returns, and processes may enter uninterruptible I/O waits depending on the OS and operation. This favors correctness and transparent recovery but expands failure domains: an unavailable NFS server can stall boot, shutdown, job workers, or any process touching the mount.

A soft mount returns an error after retry limits. This improves bounded responsiveness but exposes ambiguous outcomes and partial I/O to applications that often are not written to recover safely.

### 9.3 Throughput and latency mechanisms

NFS implementations reduce latency and raise throughput through:

- client page cache, read-ahead, and write-behind;
- larger negotiated read/write sizes;
- several outstanding RPCs and v4.1 session slots;
- v4 `COMPOUND` batching;
- TCP connection trunking or Linux `nconnect`-style parallel connections;
- delegations for uncontended files;
- `READDIRPLUS`/attribute coalescing;
- unstable writes followed by batched commit;
- pNFS direct parallel I/O to data servers;
- server-side copy/clone in v4.2;
- optional RDMA.

The cost profile depends on workload. Small metadata-heavy operations are latency/IOPS bound; large sequential I/O is bandwidth bound; shared-write workloads are coherence/locking bound. More aggressive caching helps private workloads and can hurt visibility in heavily shared ones.

## 10. pNFS and multi-server behavior

Parallel NFS (pNFS), introduced in v4.1, separates metadata coordination from file-data access:

```text
                       metadata/open/layout
client  <----------------------------------------> metadata server
  │
  ├──────── read/write using granted layout ────> data server A
  ├──────── read/write using granted layout ────> data server B
  └──────── read/write using granted layout ────> data server C
```

The metadata server grants a recallable **layout** describing where and how a file's byte ranges are stored. The client can then perform I/O directly and in parallel using the layout type's storage protocol. Layouts are stateful and need recall, return, recovery, validation, and fencing so a revoked client cannot continue unsafe I/O. The core model is in [RFC 8881](https://www.rfc-editor.org/rfc/rfc8881.html); general layout requirements are clarified by [RFC 8434](https://www.rfc-editor.org/rfc/rfc8434.html).

NFSv4 also supports a multi-server namespace through filesystem-location attributes, referrals, replication information, and migration. A server can direct a client to another location; clients must distinguish referral/migration from ordinary stale state and reconstruct access while preserving state when the protocol permits it.

pNFS is not a first implementation milestone for w9pt. It multiplies the hardest state, security, and consistency problems and should come only after a correct single-server v4.1 model.

## 11. POSIX and backend semantic gaps

The NFS protocol deliberately spans heterogeneous systems, so exact local behavior cannot always be preserved.

Common gaps and traps include:

- **Concurrent visibility:** cached clients do not have instant global coherence.
- **Atomic append:** a client may have to synthesize append behavior; concurrent appenders require server/protocol support and careful serialization.
- **Open-unlink:** clients may use silly rename or v4 state-specific support.
- **Exclusive create:** safe retry needs a verifier, not only “check then create.”
- **Rename:** a backend must provide atomic same-filesystem rename semantics or reject the operation; copy-then-delete is observably different.
- **Cross-filesystem rename:** normally fails, even when both filesystems appear under one v4 pseudo-root.
- **Hard links:** require stable shared object identity and link counts; many object backends cannot model them directly.
- **Case and Unicode:** backend case folding/normalization can disagree with client expectations. Names are protocol data, not host-language text to normalize casually.
- **Timestamps:** precision and update rules differ. v4's change attribute must reliably represent cache-relevant changes even when timestamps collide.
- **Advisory locks:** only cooperating clients are serialized, and server/client restart requires recovery.
- **Memory mapping:** page-cache/writeback timing makes concurrent shared mappings particularly sensitive to cache coherence.
- **Errors:** the server must map backend errors precisely. Retriable delay, quota, no-space, read-only, stale handle, permission, unsupported, and I/O failure are not interchangeable.
- **Durability:** local `fsync`, object-store completion, replicated quorum commit, and “placed in a process buffer” have different guarantees.
- **Extended attributes and ACLs:** support is version/extension/backend dependent and translations can lose semantics.

## 12. Linux operational baseline

The following is useful implementation context, not a substitute for the RFCs.

Server-side exports are configured with `/etc/exports`/`exportfs`. Important policy dimensions include:

- which clients may mount/access an export;
- `ro` versus `rw`;
- `sync` versus `async` durability acknowledgment;
- accepted `sec=` authentication flavors;
- `root_squash`, `all_squash`, and anonymous UID/GID mapping;
- subtree checking and filesystem-crossing behavior;
- whether protected transport is optional or required.

Client mounts select version/minor version, transport, security flavor, retry policy, read/write sizes, cache lifetimes, locking, and connection count. Particularly consequential options include:

- `hard` versus `soft`/`softerr`;
- `sec=sys|krb5|krb5i|krb5p`;
- `actimeo`, `noac`, `lookupcache`, and `cto`/`nocto`;
- `sync` and application-level `O_SYNC`/`fsync`;
- `nconnect` and transport selection;
- `nolock` for exceptional v3 cases.

Defaults vary with OS, kernel, nfs-utils, server, and negotiated version. They should be observed from the actual mount (`findmnt`, `/proc/mounts`, client/server statistics, and packet traces) rather than inferred from the command line alone. The upstream Linux user-space reference is [nfs(5)](https://man7.org/linux/man-pages/man5/nfs.5.html), and kernel-specific implementation notes are indexed under [Linux NFS documentation](https://www.kernel.org/doc/html/latest/filesystems/nfs/index.html).

## 13. Implications for w9pt

There are three materially different meanings of “support NFS.” They should not be mixed in one design discussion.

### 13.1 Option A: use an OS-mounted NFS tree as a w9pt disk backend

w9pt opens an ordinary mounted path and lets the operating system NFS client implement the protocol.

Benefits:

- smallest implementation and protocol-security surface;
- mature kernel caching, retry, locking, recovery, Kerberos, and TLS support;
- w9pt can reuse a disk/POSIX adapter.

Limits:

- behavior depends on mount options and host administration;
- not available in browsers/OPFS environments;
- NFS-specific errors and durability/coherence distinctions can be hidden behind the local API;
- tests need a real mount and must account for hard-mount stalls.

This is the recommended first path if the goal is merely “read and write data stored on NFS.” The adapter should document that backend semantics inherit the mounted filesystem and should avoid claiming stronger consistency or durability than `fsync` actually provides.

### 13.2 Option B: implement a userspace NFS client backend

w9pt speaks XDR/ONC RPC directly to an existing NFS server.

This requires much more than encoding `READ` and `WRITE`:

- RPC framing, multiplexing, timeouts, reconnection, authentication, and replay handling;
- v3 MOUNT/rpcbind and side protocols, or the v4 client/session/state machine;
- handle, attribute, directory, and data caches;
- open/lock/delegation/lease lifecycle and recovery for v4;
- id mapping, Kerberos/GSS, TLS, ACL/attribute handling;
- correct retry classification and delayed write-error reporting;
- a network transport unavailable to ordinary browser JavaScript.

A read-only, explicitly limited client is possible, but a general read/write v4.1 client is a major subsystem. Using an established NFS client library would be preferable if licensing, platform, and asynchronous-runtime constraints fit.

### 13.3 Option C: expose w9pt backends through an NFS server

w9pt becomes the storage engine behind an NFS endpoint consumed by standard clients. This is attractive for interoperability but creates a strict backend contract.

A plausible server pipeline is:

```text
connection/RPC layer
  -> XDR limits and decode
  -> authentication + export selection
  -> duplicate/session replay control
  -> current/saved handle resolution
  -> per-operation authorization
  -> namespace/state/locking layer
  -> w9pt capability adapter
  -> durability barrier
  -> attributes + error mapping + XDR reply
```

The NFS facade must expose the intersection of its guaranteed semantics, or negotiate/reject unsupported features. It must not simulate required atomic or durable behavior with a weaker backend and report success.

### 13.4 Required backend capability contract

Before designing an NFS server, define and test these capabilities independently of the wire protocol:

| Capability | Why NFS needs it | Likely fallback when absent |
| --- | --- | --- |
| Stable object ID plus generation | Persistent, non-aliasing file handles | Persistent metadata index; otherwise volatile handles with explicit limitations |
| Resolve ID without pathname | Open handles survive rename | ID-to-location index updated atomically with namespace changes |
| Atomic create-if-absent | Exclusive create and replay safety | Backend conditional operation/transaction; otherwise reject guarantee |
| Atomic same-filesystem rename | Namespace semantics | Metadata indirection transaction; copy/delete is not equivalent |
| Random `read_at`/`write_at` | NFS byte-range I/O | Range reads plus staged rewrite; advertise honest size/performance/durability constraints |
| Truncate and size update | `SETATTR`, create, write | Versioned object replacement with serialization |
| Durable commit barrier | Stable `WRITE`, `COMMIT`, `fsync` | Flush/finalize and wait for backend durability; never overstate result |
| Monotonic cache-change token | Client cache validation | Persisted per-object generation incremented on every relevant mutation |
| Stable directory continuation | `READDIR` cookies/verifier | Snapshot or opaque validated cursor; restart enumeration on invalidation |
| Atomic metadata/data coordination | Accurate size/change/write results | Transaction/log or conservative serialization |
| Authorization metadata | Per-request access checks | Server-owned metadata sidecar; reject unsupported ACL semantics |
| Advisory range locks/share state | v4 open/lock semantics | Central state manager with lease and recovery support |
| Quota/statistics reporting | `FSSTAT`/attributes and precise errors | Explicit “unknown/not supported” where protocol permits |
| Links/symlinks/xattrs/sparse ranges | Optional filesystem features | Capability negotiation and `NOTSUPP`, not lossy emulation |

### 13.5 Backend-specific pressure points

**Memory backend**

- Fast and easy to serialize within one process.
- All handles, file data, locks, reply cache, and change counters vanish on restart unless separately persisted.
- Should advertise volatile behavior or deliberately initiate server-restart recovery.

**Disk/local filesystem backend**

- Usually closest to NFS semantics and may already supply stable exportable handles through the OS.
- A user-space path adapter still needs protection against path traversal, rename races, inode reuse, and time-of-check/time-of-use bugs.
- Opening by pathname after handle resolution is not sufficient; use descriptor-relative/handle-safe operations where the platform allows.

**OPFS backend**

- Lives in a browser/worker security and API model with no direct raw TCP NFS listener/client in ordinary web code.
- Names, synchronous access handles, worker ownership, and durability need a platform-specific bridge.
- NFS would normally terminate in a native/server proxy rather than inside the browser.

**Object-storage backend**

- Keys are path-like names, not stable file identities.
- Rename is commonly copy plus delete, not atomic.
- Random writes often require multipart staging or full-object replacement.
- Directory listings are synthesized from prefixes and continuation tokens.
- Per-object metadata, conditional updates, version IDs, and listing consistency differ by provider.
- A correct facade likely needs a transactional metadata/index layer above raw objects, with immutable data chunks/manifests and conditional generation updates.

### 13.6 Backend I/O versus NFS random access

w9pt backends must support or explicitly reject concurrent, out-of-order byte-range I/O, repeated reads, retransmitted writes, truncation, and durability barriers. The semantic core should expose bounded random access and explicit durability rather than assuming one forward-only stream.

A useful separation would be:

```text
stream convenience API
        │
        ▼
filesystem semantic core
  - object identity
  - namespace atomicity
  - attributes/change generation
  - read_at/write_at/truncate/commit
  - authorization and optional locks
        │
        ▼
backend capability adapters
```

The NFS protocol layer, if ever built, should depend on that semantic core instead of embedding backend-specific exceptions in RPC handlers.

## 14. Suggested implementation/research sequence

This survey does not imply that w9pt should implement NFS now. If NFS becomes a product goal, de-risk it in this order:

1. Decide which meaning of support is required: mounted-path backend, userspace client, or server facade.
2. Write a backend capability specification for identity, namespace atomicity, cache generations, random I/O, and durability.
3. Build failure-oriented conformance tests against memory and disk adapters before adding a network protocol.
4. If consuming NFS, prototype the mounted-path option and document observed semantics first.
5. If serving NFS, choose a minimum version and feature set explicitly. A limited v3 prototype is simpler but carries auxiliary protocols and weak security; a production-oriented v4.1 design has a larger state machine but a cleaner modern architecture.
6. Implement XDR/RPC with strict resource limits and fuzzing separately from filesystem semantics.
7. Add read-only namespace/attribute/read operations, then mutation and durability, then replay/recovery, then locks/delegations.
8. Validate with at least two independent clients, packet traces, concurrent workload tests, and forced packet loss, disconnect, client restart, server restart, and backend failure.
9. Add strong authentication and required transport protection before untrusted-network deployment.
10. Treat pNFS and broad v4.2 extensions as later capability-driven work.

## 15. Open questions for this repository

- Is the actual goal to access existing NFS storage, or to let NFS clients access w9pt storage?
- Which runtimes are in scope: native server, Node/Bun/Deno, WASM, or browser?
- What semantics does w9pt promise uniformly, and what may vary by backend?
- Does an object keep identity across rename, and where is that identity persisted?
- Is concurrent multi-process/multi-host mutation in scope?
- What does `flush`/`close`/`sync` mean for each backend?
- Are permissions and identities part of w9pt's model, or supplied only by a future protocol facade?
- Can backends provide a monotonic change generation independent of timestamp precision?
- Which operations may return “unsupported” instead of being emulated?
- Is a dependency on host NFS mounts acceptable for the first use case?

## 16. Failure cases a design must test

- request executes but reply is dropped;
- duplicate non-idempotent request arrives on the same or a new connection;
- TCP disconnects with several requests in flight;
- client restarts and reuses or changes its identity;
- server restarts after acknowledging unstable and stable writes;
- server restarts during a multi-operation compound;
- session reply cache is lost or only partly persisted;
- file is renamed while handles, opens, or directory cookies exist;
- object is removed and its backend numeric ID is later reused;
- directory changes during paged enumeration;
- two clients write overlapping ranges with and without locks;
- delegation recall races with writeback or client loss;
- lease expires while the client is partitioned;
- backend quota/no-space/error appears during commit;
- credentials expire or identity mapping changes during an open lease;
- export policy changes while clients retain handles;
- malformed XDR claims huge arrays, counts, or operation lists;
- object-store conditional update loses a race;
- hard-mounted server becomes unavailable during process shutdown.

## 17. Primary references

Protocol foundations:

- [RFC 4506 — XDR](https://www.rfc-editor.org/rfc/rfc4506.html)
- [RFC 5531 — ONC RPC version 2](https://www.rfc-editor.org/rfc/rfc5531.html)
- [RFC 2203 — RPCSEC_GSS](https://www.rfc-editor.org/rfc/rfc2203.html)
- [RFC 9289 — RPC-with-TLS](https://www.rfc-editor.org/rfc/rfc9289.html)
- [RFC 8166 — RPC-over-RDMA](https://www.rfc-editor.org/rfc/rfc8166.html)

NFS versions and extensions:

- [RFC 1094 — NFSv2](https://www.rfc-editor.org/rfc/rfc1094.html)
- [RFC 1813 — NFSv3, including MOUNT v3](https://www.rfc-editor.org/rfc/rfc1813.html)
- [RFC 7530 — NFSv4.0](https://www.rfc-editor.org/rfc/rfc7530.html)
- [RFC 8881 — NFSv4.1](https://www.rfc-editor.org/rfc/rfc8881.html)
- [RFC 7862 — NFSv4.2 operations](https://www.rfc-editor.org/rfc/rfc7862.html)
- [RFC 7863 — NFSv4.2 XDR](https://www.rfc-editor.org/rfc/rfc7863.html)
- [RFC 8178 — NFSv4 extension/minor-version rules](https://www.rfc-editor.org/rfc/rfc8178.html)
- [RFC 8276 — NFSv4 extended attributes](https://www.rfc-editor.org/rfc/rfc8276.html)
- [RFC 8434 — pNFS layout-type requirements](https://www.rfc-editor.org/rfc/rfc8434.html)
- [RFC 9754 — NFSv4.2 open/delegation extensions](https://www.rfc-editor.org/rfc/rfc9754.html)
- [IETF NFSv4 working group](https://datatracker.ietf.org/wg/nfsv4/about/)

Implementation/operations references:

- [Linux `nfs(5)` client behavior and mount options](https://man7.org/linux/man-pages/man5/nfs.5.html)
- [Linux `exports(5)` server export policy](https://man7.org/linux/man-pages/man5/exports.5.html)
- [Linux kernel NFS documentation](https://www.kernel.org/doc/html/latest/filesystems/nfs/index.html)
- [Linux kernel: making filesystems exportable](https://www.kernel.org/doc/html/latest/filesystems/nfs/exporting.html)

## 18. Terminology quick reference

| Term | Meaning |
| --- | --- |
| export | A server filesystem tree made available to selected NFS clients |
| file handle | Opaque server-issued identifier for a filesystem object |
| XDR | Canonical binary data representation used on the wire |
| ONC RPC | Request/reply framework on which NFS is defined |
| compound | Ordered v4 list of operations in one RPC; not a transaction |
| stateid | v4 token identifying open/lock/delegation/layout state |
| clientid | v4 server-recognized identity for a client instance |
| lease | Time interval during which v4 client state remains protected/renewable |
| delegation | Recallable grant letting a client authoritatively cache some file behavior |
| session | v4.1 client/server object with slots, sequencing, reply caching, and channels |
| slot | Session lane with its own sequence number and cached reply |
| WCC | v3 weak cache consistency data around modifying operations |
| write verifier | Server-instance token used to detect loss of acknowledged unstable writes |
| cookie | Opaque directory-enumeration continuation value |
| pseudo-filesystem | v4 logical namespace that joins exported filesystems under one root |
| NLM / NSM | v3 side protocols for locking and reboot/status coordination |
| pNFS layout | Recallable map allowing a client to access data servers directly |
| security flavor | RPC authentication/protection mechanism selected for requests |
