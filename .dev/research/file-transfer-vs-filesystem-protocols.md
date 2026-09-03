# File transfer protocols versus network filesystem protocols

Research status: initial comparative survey  
Last checked: 2026-09-01  
Related research: [How NFS works](./nfs.md), [9P over an S3-backed filesystem](./9p-s3-filesystem.md)

## Executive summary

FTP, SFTP, WebDAV, and SMB do not provide equivalent filesystem behavior despite all being able to move files.

There are three useful categories:

1. **Transfer/synchronization protocols** — FTP, SCP, TFTP, rsync. They primarily copy whole files or synchronize trees. An application normally operates on a local copy rather than issuing ordinary file reads/writes remotely.
2. **Filesystem-like remote APIs** — SFTP and WebDAV. They have directory, metadata, and mutation operations and can be mounted through FUSE or OS clients, but do not reproduce all native filesystem semantics.
3. **Network filesystem protocols** — SMB, NFS, and 9P. They have remote open handles, offset reads/writes, locking/state, cache rules, and server-side filesystem semantics. A kernel or FUSE client translates application syscalls directly into protocol requests.

The short comparison is:

| Protocol | Primary model | Native-filesystem closeness |
| --- | --- | --- |
| TFTP | Download/upload one named file | Very low |
| SCP | Secure whole-file/tree copy | Very low |
| FTP/FTPS | File transfer plus basic remote directory management | Low |
| rsync | Efficient tree synchronization/delta transfer | Low; not a live mount protocol |
| WebDAV | HTTP resources, collections, properties, and write locks | Medium-low |
| SFTP | Handle-based remote file API over SSH | Medium; mountable through SSHFS |
| 9P2000.L | Stateful remote filesystem RPC | High for supported Linux operations |
| NFSv4 | Stateful distributed filesystem with caching/recovery | High |
| SMB2/3 | Stateful network filesystem with rich Windows semantics | Very high on Windows; high but translated on POSIX |

None is perfectly identical to a local filesystem. Network failure, server capabilities, client caching, identity mapping, locking, filename rules, durability, and platform semantics remain visible.

## 1. What happens to an application syscall?

Suppose an application runs:

```text
fd = open("file", read-write)
pread(fd, buffer, 4096, offset=1 MiB)
pwrite(fd, changed_bytes, 100, offset=2 MiB)
fsync(fd)
close(fd)
```

### Transfer-protocol workflow

With a traditional FTP/SCP workflow:

```text
download remote file
        ↓
create complete local file
        ↓
application modifies local file normally
        ↓
upload complete replacement file
```

The editor/application is not operating on the remote object. The transfer client is.

Some tools hide this by downloading to a temporary file and uploading it on save/close, but that is an application convention rather than protocol-level random filesystem I/O.

### Mounted filesystem-like workflow

With SSHFS over SFTP or a WebDAV mount:

```text
application syscall
  → kernel VFS
  → FUSE/OS mount client
  → SFTP/WebDAV requests
  → server filesystem/resource
```

The mount adapter emulates missing behavior. It may cache, stage, rewrite whole files, synthesize inode numbers/permissions, or reject unsupported operations.

### Native network-filesystem workflow

With SMB, NFS, or 9P:

```text
application syscall
  → kernel network-filesystem client
  → remote open/read/write/flush/lock RPCs
  → server filesystem
```

The protocol itself models open handles, offsets, state, and concurrency. This is the closest to a local filesystem.

## 2. FTP and FTPS

FTP is a file-transfer protocol with a long-lived textual **control connection** plus separate data connections for transfers and listings. It has more than only upload/download:

| Operation | FTP command |
| --- | --- |
| Download | `RETR` |
| Upload/replace | `STOR` |
| Unique-name upload | `STOU` |
| Append | `APPE` |
| Resume/restart transfer | `REST` followed by transfer command |
| Delete file | `DELE` |
| Rename | `RNFR`, then `RNTO` |
| Create/remove directory | `MKD`, `RMD` |
| Change/current directory | `CWD`, `CDUP`, `PWD` |
| List | `LIST`, `NLST`; structured `MLSD` extension |
| Size/time/type facts | `SIZE`, `MDTM`, `MLST` extensions |

These are standardized in [RFC 959](https://www.rfc-editor.org/rfc/rfc959.html) and [RFC 3659](https://www.rfc-editor.org/rfc/rfc3659.html).

### What FTP does not provide well

- No persistent protocol file handle comparable to an OS descriptor.
- No general `READ(offset,count)`/`WRITE(offset,data)` API on an open handle.
- `REST` supports transfer restart/resume, not a complete random-access/concurrent-write model.
- No standardized byte-range locks, share modes, or coherent client caches.
- No standard `fsync`/durability barrier.
- No portable POSIX `stat` record, inode identity, UID/GID/mode, ACL, xattr, symlink, hard-link, or sparse-file model.
- No open-unlink semantics.
- Directory listing was historically human/server-specific text; MLSD improves interoperability but remains transfer metadata.
- Rename/create atomicity and overwrite behavior depend on the server/backend.
- FTP has no standard remote change notification.

FTPS adds TLS protection to FTP. It does not add filesystem semantics. Plain FTP sends credentials/data without modern transport protection and should not be used on untrusted networks.

### Mounting FTP

FUSE FTP mounts exist, but they synthesize a filesystem from path commands and transfers. Editing a remote file commonly means staging/re-uploading it, with weak locking and failure behavior. FTP should be treated as a transfer endpoint, not a general mutable filesystem backend.

## 3. SFTP

SFTP is the **SSH File Transfer Protocol**, not FTP wrapped in SSH. It runs as an SSH subsystem and is a binary request/reply protocol. The widely interoperable OpenSSH baseline is protocol version 3 from `draft-ietf-secsh-filexfer-02`, plus vendor extensions. OpenSSH lists the draft on its [specifications page](https://www.openssh.org/specs.html).

SFTP is substantially more filesystem-like than FTP:

```text
OPEN(path, flags, attrs) → opaque handle
READ(handle, offset, length)
WRITE(handle, offset, data)
FSTAT(handle)
FSETSTAT(handle, attrs)
CLOSE(handle)
```

It also provides:

- `STAT`, `LSTAT`, and `SETSTAT` by path;
- directory `OPENDIR`, `READDIR`, and handle close;
- `REMOVE`, `MKDIR`, and `RMDIR`;
- `RENAME`;
- `REALPATH`;
- `READLINK` and `SYMLINK`;
- asynchronous request IDs, allowing multiple requests in flight.

OpenSSH adds negotiated extensions including:

- `posix-rename@openssh.com` for atomic POSIX-style rename/replace;
- `hardlink@openssh.com`;
- `fsync@openssh.com` on an open handle;
- `statvfs@openssh.com`/`fstatvfs@openssh.com`;
- `copy-data`;
- `lsetstat@openssh.com` and limits/path extensions.

See OpenSSH's [protocol extensions](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL).

### Why SFTP still is not a complete native filesystem

- The common v3 base has no broadly interoperable byte-range lock operations.
- No lease/oplock/delegation cache-coherence protocol.
- No durable open handles across SSH connection loss.
- No standardized multi-client change notification.
- UID/GID/mode fields are Unix-oriented but identity mapping and ownership changes depend on the server account.
- ACLs, xattrs, named streams, device nodes, sparse allocation, and advanced rename/fallocate behavior are incomplete or server-specific.
- Error reporting is less precise than local errno; some servers return only generic failure.
- Open-unlink, hard-link inode identity, and rename-overwrite can differ by implementation/extension.
- `mmap` behavior is provided by a mount client's page cache, not SFTP itself.
- Cross-request ordering is defined within a connection, but multiple SFTP connections require extra client coordination.

### SSHFS

[SSHFS](https://github.com/libfuse/sshfs) maps Linux FUSE operations onto SFTP and is convenient because most SSH servers already expose SFTP. It demonstrates both the usefulness and gaps:

- remote reads/writes can be offset-based rather than whole-file uploads;
- it supports caching, read-ahead, writeback, reconnect, and OpenSSH extensions;
- reconnect can lose in-flight data;
- hard links can appear as distinct inode numbers/link count one;
- rename-over-existing may need a non-atomic remove-then-rename workaround;
- connection failures can block filesystem syscalls until timeout/disconnect.

See [SSHFS caveats](https://github.com/libfuse/sshfs/blob/master/sshfs.rst).

SFTP is a good secure remote-file management API and an acceptable convenience mount. It is less suitable than SMB/NFS/9P for shared databases, strong multi-client coherence, or transparent native filesystem behavior.

## 4. SMB2 and SMB3

SMB is a true network file-sharing protocol. Microsoft describes SMB as allowing applications to read, create, write, and update remote files. Its protocol operations map closely to filesystem behavior.

An SMB2/3 flow is conceptually:

```text
NEGOTIATE dialect/capabilities
SESSION_SETUP authenticate
TREE_CONNECT share
CREATE path/access/share-mode/options → FileId
READ FileId/offset/length
WRITE FileId/offset/data
LOCK FileId/ranges
QUERY_INFO / SET_INFO
FLUSH FileId
CLOSE FileId
```

`SMB2 CREATE` means “create or open” and returns a server-side handle/FileId. The message families include:

- `CREATE`, `CLOSE`, `READ`, `WRITE`, `FLUSH`;
- `LOCK` byte ranges;
- `QUERY_INFO`, `SET_INFO`, `IOCTL`;
- `QUERY_DIRECTORY`;
- `CHANGE_NOTIFY` on a directory;
- `OPLOCK_BREAK`/lease handling for cache coherence;
- cancellation and compounding.

See Microsoft [MS-SMB2 message syntax](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/6eaf6e75-9c23-4eda-be99-c9223c60b181) and [SMB overview](https://learn.microsoft.com/en-us/windows/win32/fileio/microsoft-smb-protocol-and-cifs-protocol-overview).

### Stateful filesystem features

SMB provides features that transfer protocols lack:

- per-open requested access plus share/deny modes for read, write, and delete;
- byte-range locks;
- oplocks/leases that let clients cache reads, writes, and handles until a conflicting accessor appears;
- directory change notifications;
- security descriptors and rich Windows ACLs;
- file attributes, timestamps, allocation/EOF sizes;
- alternate data streams and extended attributes depending on server/client;
- durable and persistent handles that can reconnect after connection/server failover when supported;
- atomic server-side rename/delete semantics as supported by the backend;
- SMB3 signing, encryption, multichannel, scale-out/continuous-availability features, and optional RDMA transports.

SMB handles open/delete behavior explicitly. Windows-style delete-pending and share-delete rules can preserve open handles while controlling name visibility.

### Native mount behavior

Windows applications generally use SMB through the normal filesystem API with strong semantic fidelity. Linux/macOS SMB clients translate between POSIX APIs and Windows/SMB semantics.

SMB still is not identical everywhere:

- Windows share modes and delete semantics differ from POSIX defaults.
- Case sensitivity and forbidden/reserved names differ.
- UID/GID/mode, POSIX ACLs, symlinks, hard links, device files, and xattrs depend on server dialect/extensions and mount settings.
- A Samba/Linux server and a Windows server can expose different behavior under the same SMB dialect.
- Client caching and leases must be recalled across conflicts and failures.
- Network outage and server failover remain observable.

SMB is the closest choice here to a full remote native filesystem, especially for Windows compatibility, but is much more complex to implement than 9P or a limited SFTP server.

## 5. WebDAV

WebDAV extends HTTP's resource model with distributed-authoring operations:

| Behavior | HTTP/WebDAV method |
| --- | --- |
| Download | `GET` |
| Create/replace resource | `PUT` |
| Delete | `DELETE` |
| Inspect/update properties | `PROPFIND`, `PROPPATCH` |
| Create collection/directory | `MKCOL` |
| Copy/move/rename | `COPY`, `MOVE` |
| Resource write locks | `LOCK`, `UNLOCK` |

These are specified by [RFC 4918](https://www.rfc-editor.org/rfc/rfc4918.html).

WebDAV is richer than FTP for remote authoring and integrates well with HTTPS, proxies, and HTTP identity. Many operating systems can mount it.

However:

- `PUT` normally replaces the complete resource; there is no universal random offset-write method.
- HTTP range requests help partial reads, not generic partial mutation.
- Locks are resource/write locks with tokens/timeouts, not POSIX byte-range locks.
- No persistent OS-style open file handle is required.
- Native inode identity, hard links, symlinks, ownership, modes, ACL translation, xattrs, sparse files, mmap coherence, and open-unlink behavior are incomplete/server-specific.
- `MOVE`/`COPY` behavior and atomicity can vary across resources/backends.
- Clients often cache/stage whole files, and interoperability varies.

WebDAV is well suited to document authoring and browser/HTTP environments, but not to applications expecting full native filesystem semantics.

## 6. SCP, TFTP, and rsync

### SCP

SCP is a secure copy mechanism using SSH transport. It copies files/directories and selected metadata. It does not expose a reusable remote open/read/write/lock filesystem API. Modern OpenSSH's `scp` command commonly uses SFTP internally, but the user-facing workflow remains copy-oriented.

### TFTP

TFTP is deliberately minimal: read request or write request for one named file, transferred in blocks over UDP. It has no directory listing, rename, delete, authentication, ownership, or general filesystem operations. It is useful for boot firmware/config transfer on controlled networks.

### rsync

rsync compares directory trees and efficiently transfers changed file regions/metadata. It can preserve permissions, times, links, and delete destination files when requested. It produces a synchronized local tree; it is not a live mounted filesystem protocol and provides no remote open handle, locks, cache coherence, or application syscall path.

## 7. Feature matrix

Legend: **yes** = protocol-native/common; **limited** = extension/emulation/server-dependent; **no** = not the protocol model.

| Feature | FTP | SFTP v3 + OpenSSH | WebDAV | SMB2/3 |
| --- | --- | --- | --- | --- |
| Download/upload complete file | Yes | Yes | Yes | Yes |
| Directory list/create/remove | Yes | Yes | Yes | Yes |
| Rename/delete | Yes | Yes | Yes | Yes |
| Persistent remote open handle | No | Yes | No/limited lock token | Yes |
| Random offset read | Limited resume | Yes | HTTP range read | Yes |
| Random offset write | No general model | Yes | No general model | Yes |
| Append | `APPE` | Open/write at EOF | Whole-resource conventions | Yes |
| Truncate by size | No portable command | `SETSTAT` size | Replace resource | Yes |
| `stat`-like metadata | Limited facts | Unix-shaped attrs | Resource properties | Rich native metadata |
| chmod/chown | Nonstandard `SITE` | Limited/server identity | Server-specific properties | ACL/security-info; POSIX mapping varies |
| Symlink/hard link | Nonstandard | Symlink base; hardlink extension | Generally no | Supported depending backend/dialect |
| Byte-range locks | No | Not common v3 baseline | No; resource write locks | Yes |
| Cache coherence leases | No | No | No general equivalent | Yes, leases/oplocks |
| Flush/fsync durability | No | OpenSSH extension | No portable equivalent | `FLUSH` |
| Directory change notify | No | No common baseline | No core live watch | Yes |
| Reconnect open handle | No | No standard durable handle | Lock token may persist, not descriptor | Durable/persistent handles |
| Kernel/native mount | Third-party FUSE | SSHFS/FUSE | OS/FUSE clients | Native kernel/OS clients |
| Multi-client shared-write fidelity | Low | Medium-low | Low | High |

## 8. Why a mounted transfer protocol may look deceptively complete

A FUSE adapter can expose every VFS callback even when the wire protocol cannot implement it faithfully. The adapter must choose among:

- emulate with several remote operations;
- stage locally and upload on close;
- synthesize attributes/inodes;
- cache and accept staleness;
- use vendor extensions;
- return `EOPNOTSUPP`;
- silently weaken atomicity or durability.

Examples:

- FTP rename-over-existing may become delete then rename, creating a race.
- WebDAV editing may download and later `PUT` the full file.
- SSHFS hard links can appear as unrelated inode numbers.
- SSHFS reconnect can restore transport access but lose an in-progress write.

“Can be mounted” is therefore not equivalent to “has native filesystem semantics.”

## 9. Recommendations for the w9pt framework

### If the goal is full filesystem access

- Use a real filesystem semantic core and expose 9P, NFS, or SMB.
- 9P is the simplest protocol to implement but has limited stock client reach/recovery.
- NFS has broad Unix infrastructure and mature caching/recovery, with substantial complexity.
- SMB gives the richest Windows/native file-sharing behavior and broad client support, with the largest implementation/security surface.

### If the goal is secure remote file management

SFTP is attractive. It is far easier to deploy through existing SSH infrastructure and supports offset I/O and basic filesystem mutations. Document that it is not a strong shared-filesystem coherence protocol.

### If the goal is browser/document access

Use HTTPS/WebDAV or a purpose-built HTTP API. A 9P-over-WebSocket client is appropriate when the browser needs the same semantic core as native clients.

### If the goal is transfer/synchronization

Use SFTP, rsync, or object-storage APIs rather than pretending they are mounted filesystems.

### Architectural separation

```text
protocol adapters
  ├── 9P / NFS / SMB       full filesystem clients
  ├── SFTP                 remote management/transfer
  ├── WebDAV/HTTP          browser/document workflows
  └── sync/import/export   FTP/rsync/object storage
             │
             ▼
filesystem semantic core
  inodes, directories, attrs, random I/O,
  transactions, locks, durability, GC
             │
             ▼
w9pt backend adapters
```

Do not let the least capable transfer protocol define the internal filesystem semantics. Each adapter should expose the operations it can preserve and reject or clearly document the rest.

## 10. Primary references

- [RFC 959 — FTP](https://www.rfc-editor.org/rfc/rfc959.html)
- [RFC 3659 — FTP extensions](https://www.rfc-editor.org/rfc/rfc3659.html)
- [OpenSSH SFTP v3 specification pointer](https://www.openssh.org/specs.html)
- [OpenSSH SFTP extensions](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL)
- [SSHFS](https://github.com/libfuse/sshfs)
- [SSHFS caveats](https://github.com/libfuse/sshfs/blob/master/sshfs.rst)
- [Microsoft SMB overview](https://learn.microsoft.com/en-us/windows/win32/fileio/microsoft-smb-protocol-and-cifs-protocol-overview)
- [MS-SMB2 message families](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/6eaf6e75-9c23-4eda-be99-c9223c60b181)
- [MS-SMB2 CREATE/open](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/e8fb45c1-a03d-44ca-b7ae-47385cfd7997)
- [RFC 4918 — WebDAV](https://www.rfc-editor.org/rfc/rfc4918.html)
- [RFC 1350 — TFTP](https://www.rfc-editor.org/rfc/rfc1350.html)
- [rsync project documentation](https://rsync.samba.org/documentation.html)
