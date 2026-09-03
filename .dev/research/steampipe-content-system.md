# SteamPipe's chunk-based content system

Research status: initial survey  
Last checked: 2026-09-01  
Related research: [How NFS works](./nfs.md)

## Executive summary

The system remembered here is **SteamPipe**, Steam's current game/application content distribution system. Valve's terminology is “chunks,” not filesystem blocks.

SteamPipe scans each file in a depot and divides it into chunks of roughly 1 MiB. It attempts to keep chunks that match the previous depot build, then compresses, encrypts, and uploads only new chunks. A versioned depot manifest describes the files and chunks needed for that version. For an initial installation, a client needs all chunks referenced by the selected depots. For an update, it follows the new manifest, reuses matching local data, downloads the missing/new chunks, reconstructs changed files in staging, and commits the new files.

Two corrections to the simplified mental model are important:

1. The publisher-side SteamPipe builder determines the chunk layout and reuse while producing a new depot manifest. The Steam client is not documented as independently running a generic block-diff algorithm over the entire installed game.
2. SteamPipe chunks are primarily content-delivery and patch units. Modern Steam installations are normal files under the library, not a permanent user-visible block store. Valve's older, retired GCF system did keep game files in a block-oriented cache container and may be part of the memory behind the question.

Valve publicly documents SteamPipe and distributes official binary tools through SteamCMD and the Steamworks SDK. I found **no Valve-published source code** for the core SteamPipe chunking/matching algorithm, ContentBuilder, Steam client patcher, Master Depot Server, or CDN service. Valve also does not publish a complete, stable SteamPipe wire-format specification. The exact default boundary-selection and matching algorithm remains proprietary; Valve's documentation even refers partners to an alternative build algorithm available through a Valve representative.

There are useful open-source **community implementations**, but they are not Valve code:

- SteamRE's SteamKit2 parses depot manifests and talks to Steam's content network.
- SteamRE's DepotDownloader uses SteamKit2 to download, verify, install, and update depots.
- ValvePython's `steam` package has a Python CDN client despite its name not being an official Valve project.
- TEK Steam Client is a newer partial C/C++ client and explicitly uses its own update algorithms.

For w9pt, the valuable reusable idea is not Steam compatibility. It is an immutable snapshot manifest over content-addressed chunks, plus local reuse, parallel fetch, verification, staging, and atomic activation. That model fits versioned software/assets and object storage well, but it is a different abstraction from NFS and from a mutable general-purpose filesystem.

## 1. Vocabulary and hierarchy

Steam's content hierarchy is approximately:

```text
application (AppID)
  └── build (BuildID)
      ├── depot A -> manifest version X
      ├── depot B -> manifest version Y
      └── depot C -> manifest version Z

depot manifest
  ├── file path + file metadata
  │   └── ordered chunk references
  ├── file path + file metadata
  │   └── ordered chunk references
  └── ...

chunk store / CDN
  ├── chunk identifier -> encrypted, compressed bytes
  ├── chunk identifier -> encrypted, compressed bytes
  └── ...
```

### Application

A Steam application has an AppID. A game, tool, dedicated server, DLC, soundtrack, or shared runtime can be modeled as an application.

### Depot

A depot is a logical collection of files. An app build can reference multiple depots, commonly separated by operating system, architecture, language, DLC, dedicated-server content, or content shared by several apps. The user's licenses, platform, language, and selected branch determine which depots apply.

### Build and branch

A completed upload receives a global BuildID. Each branch points at a build, and the build in turn selects a manifest version for each included depot. Public and private beta branches make it possible to promote or roll back an immutable set of depot versions without uploading all data again.

### Manifest

Valve describes a manifest as the file listing and metadata for one depot build. Officially documented file metadata includes file size, SHA-1, and flags. Each completed depot manifest has a unique 64-bit manifest ID. See Valve's [Builds documentation](https://partner.steamgames.com/doc/store/application/builds).

Community implementations show the operational manifest as a file tree whose regular-file entries contain ordered chunk records. Each record carries enough information to retrieve, verify, and place a chunk in the output file. This detail is supported by the public SteamKit2 parser, but it is a reverse-engineered implementation detail rather than a Valve compatibility promise.

### Chunk

A chunk is a bounded byte range of one file, normally around 1 MiB before compression. Chunks are compressed and encrypted independently for delivery. In the community-parsed manifest format, a chunk has:

- a 20-byte SHA-1-derived chunk identifier (`ChunkGID`);
- a checksum;
- its output offset within the file;
- compressed length;
- uncompressed length.

See SteamKit2's [manifest parser](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Types/Manifest.cs). These fields explain how a downloader can fetch chunks concurrently and write them to their correct positions.

### Are file boundaries preserved?

Yes. SteamPipe chunks **each file independently**. It does not first concatenate every file in a depot or folder into one continuous byte stream.

Given three files, the logical result is:

```text
A -> A1, A2, A3
B -> B1, B2, B3
C -> C1, C2
```

It is not logically:

```text
A || B || C -> Block1, Block2, Block3, Block4, ...
```

For example, approximate sizes might produce:

```text
A (2.4 MiB)
  A1: file offset 0.0 MiB, length about 1.0 MiB
  A2: file offset 1.0 MiB, length about 1.0 MiB
  A3: file offset 2.0 MiB, length about 0.4 MiB

B (0.5 MiB)
  B1: file offset 0, length about 0.5 MiB

C (1.2 MiB)
  C1: file offset 0.0 MiB, length about 1.0 MiB
  C2: file offset 1.0 MiB, length about 0.2 MiB
```

The manifest structure makes the boundary explicit: each file entry owns its list of chunks, and every chunk offset is relative to that file. No chunk spans the end of `A` and the beginning of `B`. Directories are metadata entries and contain no data chunks.

The CDN can still store all encrypted chunk blobs in a flat hash-addressed namespace. Thus the **physical store** may look like an unordered collection of blocks, but the **logical recipe** remains `file -> ordered chunks`. If identical chunk content appears more than once, its content identifier can reveal reuse, but Valve does not publicly specify every scope in which ContentBuilder deduplicates such repetitions.

“About 1 MiB” is not a guarantee that boundaries are always exactly `0`, `1 MiB`, `2 MiB`, and so on. Manifest records have explicit, potentially variable lengths, and Valve says the builder tries to retain matches with the previous build. The exact boundary and matching algorithm is private. What is established is that chunking starts from individual files and remains sensitive to changes that shift or reorder bytes within those files.

### Does the file-to-chunk map require a database?

The mapping must be stored persistently, but it does **not** require SQLite or any other database. The manifest itself is the authoritative metadata store.

A minimal content store can consist only of immutable files or objects:

```text
store/
  chunks/
    ab/abcdef...          # chunk bytes, key derived from content hash
    42/42c913...
  manifests/
    8f35....cbor          # immutable snapshot manifest
  refs/
    stable               # contains the active manifest hash/ID
```

Conceptually, the manifest contains:

```json
{
  "version": 1,
  "files": [
    {
      "path": "A",
      "size": 2516582,
      "hash": "...",
      "chunks": [
        { "hash": "a1...", "offset": 0,       "length": 1048576 },
        { "hash": "a2...", "offset": 1048576, "length": 1048576 },
        { "hash": "a3...", "offset": 2097152, "length": 419430 }
      ]
    },
    {
      "path": "B",
      "size": 524288,
      "hash": "...",
      "chunks": [
        { "hash": "b1...", "offset": 0, "length": 524288 }
      ]
    }
  ]
}
```

JSON is shown only for readability. A real manifest can be canonical CBOR, Protobuf, FlatBuffers, a custom binary format, or a sorted/memory-mappable index. The essential properties are deterministic encoding, explicit format/chunker versions, integrity protection, and an authenticated snapshot identity.

Publishing can remain database-free:

1. Write every new immutable chunk under its content hash.
2. Write the immutable manifest only after all its chunks are durable.
3. Verify/authenticate the manifest.
4. Atomically replace the branch/channel ref with the new manifest ID.

The client only needs to remember its currently installed manifest ID. To update, it compares the installed and target manifests, reuses local chunks, and retrieves missing hashes.

SQLite becomes useful, but optional, for operational metadata:

| Need | Manifest/files-only approach | SQLite approach |
| --- | --- | --- |
| Resolve one path | Parse or use a sorted manifest index | Indexed lookup |
| Publish a snapshot | Immutable manifest plus atomic ref swap | Metadata transaction |
| Resume downloads | Small journal/state file | Download/task tables |
| Track verified cache entries | Rebuild by scanning cache | Verification/access-time table |
| Garbage collection | Mark reachable chunks by scanning retained manifests | Reachability/refcount index |
| Concurrent publishers | Lock or conditional ref update | Transaction plus locking |
| Query millions of file/chunk records | Sharded or memory-mapped manifest | Paginated indexed queries |

Even when SQLite is used, chunk payloads should normally remain separate immutable files/objects. SQLite stores indexes and state; putting multi-gigabyte chunk blobs into one mutable database file makes CDN delivery, cache eviction, parallel access, and object-store storage harder.

For w9pt, the portable design should make immutable manifests and chunks the source of truth. A SQLite index can be an optional, rebuildable acceleration layer for native disk deployments. Making SQLite authoritative would complicate memory, browser/OPFS, and object-storage backends, and a SQLite file should not be placed directly on an object store or shared by uncoordinated remote writers.

Steam's public model requires depot manifests, installed-manifest state, and chunk storage. Valve does not publish enough Steam client internals to claim that no internal database is used anywhere, but SQLite is not required by the manifest/chunk architecture or its public protocol.

## 2. Publisher/build-side flow

Valve documents this build sequence in [Uploading to Steam](https://partner.steamgames.com/doc/sdk/uploading):

1. A publisher defines an application build and file mappings for each depot using VDF build scripts.
2. SteamCMD authenticates a build account and registers the build with the Master Depot Server (MDS).
3. For each depot, ContentBuilder generates the selected file list.
4. It scans each file and divides it into chunks of about 1 MiB.
5. If a prior build exists, the partitioning attempts to preserve as many unchanged chunks as possible.
6. New chunks are compressed and encrypted.
7. Only those new chunks are uploaded.
8. ContentBuilder generates a new depot manifest with a unique 64-bit ID.
9. MDS completes the app build and assigns a BuildID.
10. An authorized publisher promotes that build to a public or beta branch.

The builder keeps a local chunk cache and intermediate build output. Valve says deleting these does not affect correctness, but makes the next build slower because more content must be rescanned/reprocessed.

### What “diff” means here

Valve calls SteamPipe's patching algorithm a binary-delta system, but its public explanation is centered on matching chunks:

```text
old file:  [A][B][C][D][E]
new file:  [A][B][X][D][E]
upload:          [X]
```

The manifest for the new build refers to the reusable chunk identities for `A`, `B`, `D`, and `E`, plus the newly uploaded identity for `X`.

This is not a source-code diff and it does not understand game assets. SteamPipe sees bytes and file/chunk boundaries. A manifest is closer to a recipe for reconstructing a snapshot than to a patch program that mutates an old file in place.

### Exact chunk-boundary algorithm: not public

Valve publicly says:

- chunks are roughly 1 MiB;
- the builder tries to preserve old matching chunks;
- 1 MiB pack-file alignment improves updates;
- moving sub-megabyte assets often prevents old chunks from matching;
- a small offset change spread through a pack-file table of contents can create many new 1 MiB chunks;
- an alternative build algorithm exists for problematic pack files, but partners must contact Valve to use it.

That is insufficient to reproduce the builder bit-for-bit. It suggests that the common path is materially position-sensitive, but it does not establish whether every boundary is fixed, how previous chunks are searched, how boundaries are shifted, what rolling/checksum algorithm is used, or when the alternate algorithm applies. This note therefore does not label SteamPipe as either a pure fixed-size chunker or a conventional content-defined chunker.

## 3. Initial installation flow

At a high level, the Steam client:

1. Resolves the app's current branch/build metadata.
2. Selects the applicable depots for platform, architecture, language, DLC, and entitlements.
3. Resolves and fetches each selected depot's manifest.
4. Obtains authorization and the required depot decryption key.
5. Builds a queue of all required chunks that are not already safely reusable locally.
6. Fetches encrypted/compressed chunks from content servers over HTTP(S), normally in parallel.
7. Decrypts, decompresses, and verifies each chunk.
8. Writes chunks into their manifest-defined file offsets in a staging area.
9. Applies file flags/permissions and installation metadata.
10. Atomically moves or commits staged files into the live game directory.

Valve's official documentation establishes the HTTP delivery, chunk encryption/compression, and final placement. Authentication, manifest parsing, per-chunk fields, and much of the concrete download pipeline are visible in the community SteamKit2/DepotDownloader implementations rather than in a complete official protocol specification.

## 4. Update flow

For an update from manifest `M1` to `M2`, the conceptual client plan is:

```text
M1 + current files                         M2
       │                                   │
       └──────── compare file/chunk recipes┘
                         │
                 ┌───────┴────────┐
                 │                │
          reusable old data   missing/new chunks
                 │                │ HTTP download
                 └───────┬────────┘
                         ▼
                 reconstruct staging files
                         │ verify
                         ▼
                   commit new snapshot
```

Typical decisions are:

- New path: construct it from all referenced chunks.
- Removed path: delete it when committing the new depot state.
- Unchanged whole-file hash: retain the installed file.
- Changed file with shared chunks: copy/reuse matching old byte ranges and download only new chunks.
- Missing, modified, or corrupt local data: download replacement chunks even if the old manifest would otherwise allow reuse.

Steam's publisher documentation explicitly says the official client constructs a changed pack file alongside the old one, then deletes the old file and moves the new one into place. It does not simply overwrite the few changed bytes in the live file.

### Network savings versus disk work

Chunk reuse minimizes transfer but can amplify local I/O and temporary space. Valve gives the example that changing 10 bytes in a 25 GiB pack file may require only a tiny network download, yet the client still constructs a new 25 GiB file and copies nearly all unchanged data locally.

This distinction matters for system design:

```text
download size != bytes read locally != bytes written locally != temporary space
```

A patch system should estimate and report all four.

### Atomicity and offline availability

Staging new files before activation has two benefits:

- the currently installed game remains coherent while download/reconstruction is incomplete;
- a failed or interrupted update can resume or roll back without leaving half-patched live files.

Valve lists continued offline availability after an update starts as a SteamPipe feature. This follows naturally from immutable manifests plus staged activation.

## 5. Why some Steam updates are unexpectedly large

SteamPipe cannot preserve chunks whose bytes or boundaries have changed. Common causes are:

- Asset reordering inside a pack file.
- Inserting bytes near the beginning and shifting all later content.
- A table of contents with absolute offsets distributed through the pack.
- Build timestamps, generated IDs, or nondeterministic ordering embedded throughout the output.
- Whole-pack compression, where one small input change alters a large compressed suffix.
- Whole-pack encryption with modes/settings that cause changes to propagate.
- Changing a shared compression dictionary or other build-wide metadata.

Valve's recommended packaging practices include:

- localize modifications within a pack;
- avoid reordering assets;
- keep pack files to roughly one or two GiB rather than tens of GiB;
- group packs by level/feature and add new packs for new features;
- keep one centralized table of contents where possible;
- remove filenames and build timestamps that create irrelevant changes;
- compress per asset rather than across asset boundaries;
- align Unreal pack data to 1 MiB when using the documented SteamPipe-oriented settings.

These recommendations reveal an important design lesson: efficient chunk distribution begins with deterministic, patch-friendly producer formats. A sophisticated downloader cannot recover reuse after the build process has randomized or globally shifted most bytes.

## 6. Storage and security model

SteamPipe content servers store compressed, encrypted chunk files. A local content server (LCS) used during development is essentially an HTTP server containing the same kind of chunk files; only metadata for local depots is sent to Steam. Valve says a user who obtains LCS chunk files still needs the depot key to decrypt them. See [SteamPipe Local Content Server](https://partner.steamgames.com/doc/sdk/uploading/local_content_server).

The layers have different jobs:

- Chunk identity/checksum: integrity and reuse lookup.
- Compression: transfer/storage reduction.
- Depot encryption key: prevents unauthorized plaintext access to obtained chunk blobs.
- Steam authentication, licenses, manifest request codes, and CDN tokens: determine which account may discover/fetch content.
- Signed/trusted application metadata: determines which manifest/version the client should install.

Content addressing alone is not authenticity. A new implementation should use a modern strong content hash, authenticate manifests, bind them to application/version identity, and reject rollback when policy requires it. SHA-1 fields should be treated as Steam compatibility data, not as a recommendation for a new w9pt format.

## 7. Was Valve's implementation published?

### Short answer

**Official tools are distributed; the core source is not.**

As of 2026-09-01, I found:

| Component | Public documentation | Official binary/tool access | Valve source code found? |
| --- | --- | --- | --- |
| SteamPipe concepts and publishing workflow | Yes | N/A | No implementation source |
| ContentBuilder/chunker/uploader | High-level behavior and VDF interface | Yes, through SteamCMD/Steamworks SDK | No |
| Steam client downloader/updater | High-level behavior and client release notes | Yes, proprietary Steam client/SteamCMD | No |
| Local Content Server tooling | Setup and behavior documented | Yes, Steamworks SDK | No reference implementation found |
| Manifest/chunk wire format | Partial concepts only | Consumed by official tools | No complete official specification or parser source found |
| Master Depot Server/CDN backend | Operationally described | Hosted service | No |

Evidence checked:

- Valve's [SteamPipe upload documentation](https://partner.steamgames.com/doc/sdk/uploading) describes algorithms at a behavioral level and instructs developers to run the self-updating `steamcmd` executable.
- Valve's [Steamworks SDK contents](https://partner.steamgames.com/doc/sdk) list ContentBuilder, ContentServer, and SteamPipeGUI tools, not their source code.
- Valve's 2011 announcement, [Download Better, Stronger, Faster](https://store.steampowered.com/news/5856/), describes the new client/server code and difference-only downloads without releasing it.
- A current scan of the public [ValveSoftware GitHub organization](https://github.com/ValveSoftware) found Steam runtime, Proton, SteamOS, and several SDK/plugin repositories, but no SteamPipe, depot builder, SteamCMD, or Steam client source repository. `steam-for-linux` is an issue tracker, not the Steam client source.
- The official documentation's undisclosed “alternate build algorithm” is further evidence that the matching implementation is not fully specified publicly.

Absence cannot be proven globally, so the precise conclusion is: **no official source release or complete public specification was found in Valve's documented SDK or public organization.**

### Publicly usable is not open source

SteamCMD can be downloaded and automated, and Steamworks partners receive ContentBuilder tools. That makes the implementation available for use under Valve's terms, but not available for inspection, modification, or redistribution as open-source code.

## 8. Community implementations

These projects are useful evidence and reference code, but none is an official SteamPipe implementation published by Valve.

### SteamKit2

[SteamRE/SteamKit](https://github.com/SteamRE/SteamKit) is an LGPL-2.1 .NET library for interoperating with the Steam network. Its repository identifies reverse engineering as a project topic. Relevant parts include:

- Steam authentication and client messaging;
- depot keys and content-server discovery;
- manifest download/decryption/parsing;
- chunk download and decoding;
- public manifest structures showing hashes, offsets, and sizes.

It is the most useful lower-level reference for the client-facing protocol. It does not reproduce Valve's private publishing backend or prove the exact ContentBuilder chunking algorithm.

### DepotDownloader

[SteamRE/DepotDownloader](https://github.com/SteamRE/DepotDownloader) is a GPL-2.0 command-line downloader built on SteamKit2. It can select app/depot/manifest versions, validate content, compare an installed manifest, queue chunks, stage files, download chunks concurrently, and remove obsolete files.

It is the most directly readable example of a Steam-compatible install/update client. Its code should be studied with its GPL license and Steam account/content entitlements in mind. It is not an official client and is not a SteamPipe build uploader.

### ValvePython `steam`

[ValvePython/steam](https://github.com/ValvePython/steam) is an MIT-licensed Python package with Steam client and CDN/depot access. “ValvePython” is a community organization name, not evidence of Valve ownership. It is useful for scripting and protocol exploration, though its supported Python versions and current maintenance should be evaluated before adoption.

### TEK Steam Client

[teknology-hub/tek-steamclient](https://github.com/teknology-hub/tek-steamclient) is a GPL-3.0 partial Steam client in C/C++. It exposes APIs for SteamPipe manifests, patches, chunks, installation, updating, and verification. The project explicitly says it is unaffiliated with Valve and uses homemade update algorithms, so its disk behavior is not a reproduction of Valve's updater.

### What remains missing

The open implementations above cover much of the **consumer/client side**. I did not find a maintained, Steam-compatible open-source replacement for Valve's publisher-side ContentBuilder algorithm and authenticated build-ingest service. Publishing a real game to Steam still uses Valve's SteamCMD/Steamworks tools.

## 9. SteamPipe versus the older GCF system

The older Steam content system used `.gcf` (Game Cache File, historically Grid Cache File) containers. GCF presented a virtual file hierarchy while storing content in an allocated/fragmented block structure inside a large cache file. Its format was proprietary and was reverse engineered by community tools such as GCFScape; the associated [HLLib source](https://github.com/mikkokko/HLLib) remains available and includes GCF validation and defragmentation support.

SteamPipe replaced that model for current distribution. Its important differences are:

| Older GCF-era model | SteamPipe model |
| --- | --- |
| Game content retained in cache container files | Game content installed as normal files |
| Local container has block allocation/fragmentation structures | CDN/build pipeline uses per-file chunks and manifests |
| Older Steam-specific transfer/storage stack | HTTP-friendly content delivery |
| Obsolete and reverse engineered | Current documented publishing system |

If the remembered detail was “Steam stores the installed game itself as blocks,” it likely refers to GCF. If it was “updates download only changed blocks,” it refers more closely to SteamPipe. They are related generations, not the same implementation.

## 10. SteamPipe is not NFS

SteamPipe and NFS solve different problems:

| SteamPipe | NFS |
| --- | --- |
| Publishes immutable versioned content snapshots | Exposes a live mutable remote filesystem |
| Optimized for install/update/read-mostly assets | Supports interactive reads, writes, rename, locks, and attributes |
| Manifest selects content-addressed chunks | File handles identify current server objects |
| Client activates a complete new version | Client operations immediately target server state |
| Stale versions are valid rollback targets | Stale handles/state are generally errors/recovery events |
| CDN and HTTP cache friendly | Stateful coherence, authorization, and RPC behavior |

A SteamPipe-style store can sit underneath a read-only/versioned filesystem, but it does not provide the mutation, locking, ownership, or coherence semantics required by an NFS server.

## 11. Design pattern relevant to w9pt

The reusable architecture is:

```text
immutable chunk blobs
  key = strong hash(uncompressed bytes)

file recipe
  path, mode, size, whole-file hash
  ordered [(chunk hash, file offset, length)]

snapshot manifest
  version, parent, files, deletions, metadata
  authenticated hash/signature

active pointer
  atomically selects one complete snapshot
```

### Publish

1. Enumerate a deterministic input tree.
2. Chunk file data using a versioned algorithm and parameters.
3. Hash each raw chunk with a modern strong digest.
4. Compress each chunk independently.
5. Upload blobs with create-if-absent semantics.
6. Generate and authenticate the immutable manifest.
7. Atomically move a channel/branch pointer to the manifest.

### Install/update

1. Fetch and authenticate the target manifest.
2. Compare it with the installed manifest and local verification cache.
3. Reuse chunks from existing files or a local content-addressed cache.
4. Fetch missing chunks concurrently and resume safely.
5. Verify before trusting or writing each chunk.
6. Reconstruct changed files in staging.
7. Verify complete file recipes or the snapshot root.
8. Atomically activate the snapshot.
9. Garbage-collect unreferenced cache data later.

### Fixed versus content-defined chunks

SteamPipe's exact algorithm is unavailable, so w9pt should choose based on its own workload:

| Fixed-size chunks | Content-defined chunks (CDC) |
| --- | --- |
| Simple, fast, direct offset math | Boundaries follow byte content via a rolling/gear hash |
| Excellent for in-place changes with stable offsets | Preserves reuse after insertions/deletions shift later data |
| A small insertion can invalidate every later boundary | More CPU and more complex boundary/version rules |
| Predictable memory and request sizes | Variable sizes need min/average/max bounds |

For already partitioned game assets with deterministic pack alignment, fixed chunks can be sufficient. For arbitrary files and object-storage snapshots, CDC is often more robust. Either choice must be versioned in the manifest; changing chunk parameters can destroy reuse between releases.

### Public systems worth studying for the generic design

These do not implement SteamPipe, but they publish the relevant algorithms:

- [casync](https://github.com/systemd/casync) combines content-defined chunking, a hash-keyed chunk store, indexes, compression, HTTP delivery, and directory-tree serialization. Its original implementation is no longer very active, but its architecture is unusually close to the desired snapshot/chunk model.
- [zchunk](https://github.com/zchunk/zchunk) is a BSD-licensed chunked compressed-file format designed to download only changed chunks using ordinary HTTP range requests.
- [restic/chunker](https://github.com/restic/chunker) exposes a well-tested Go rolling-hash chunker with a default average chunk size of 1 MiB.

These are better sources for a new open implementation than attempting to reproduce a private Steam algorithm exactly.

## 12. Engineering concerns SteamPipe's simple description hides

### Determinism

Two builds from identical inputs should produce identical file bytes, chunk boundaries, and manifests apart from explicitly excluded metadata. Sort directory walks, normalize allowed metadata, avoid wall-clock timestamps, and pin compression/chunking versions.

### Integrity and authenticity

Verify chunks on raw content, verify complete files or a Merkle root, and authenticate the manifest/channel pointer. Hashing detects accidental corruption; signatures or a trusted authenticated control plane establish publisher authenticity.

### Encryption

Convergent/content-derived encryption leaks equality and creates key-management risks. Ordinary randomized authenticated encryption prevents simple shared ciphertext deduplication. Decide whether confidentiality, cross-tenant deduplication, or per-application access isolation has priority; do not copy Steam's legacy-compatible crypto choices blindly.

### Garbage collection

Immutable chunks accumulate. Safe deletion needs a reachability scan or trustworthy reference accounting across every retained manifest, branch, rollback, pinned client version, and in-progress upload. Publication must never expose a manifest before all referenced chunks are durable.

### Cache correctness

A local chunk cache must bind data to its hash, compression/encryption format version, and verified state. Do not trust path/mtime alone. Corrupt chunks should be evicted and fetched from an alternate source.

### Atomic activation

The live application must never observe a mixture of manifests. Use atomic directory/pointer switching where possible, or a journaled commit plan with crash recovery. Preserve the old snapshot until the new snapshot is fully verified and activated.

### Disk-space planning

Preflight the download size, staging size, old-data copy/read volume, final installed size, and cache growth separately. Reflinks or clone-range operations can reduce local amplification on capable filesystems but need a copying fallback.

### Parallelism and backpressure

Concurrent HTTP fetches improve throughput, but bound memory, open files, staging writes, decompression work, and per-host requests. Schedule chunks to keep both network and disk busy without random-write thrashing.

## 13. Conclusions for w9pt

- The remembered system exists and is SteamPipe.
- Its public behavior is well documented: approximately 1 MiB per-file chunks, reuse against a previous depot build, independent compression/encryption, HTTP delivery, versioned manifests, staged reconstruction, and atomic commit.
- The exact chunking/matching implementation is not public and should not become a dependency for w9pt's design.
- Valve provides official closed binary tools, not an open-source reference implementation.
- SteamKit2 and DepotDownloader are the strongest public references for how the client-facing manifest/chunk system works, subject to their community status and licenses.
- A w9pt-native design should use immutable manifests and content-addressed chunks with modern hashes, authenticated metadata, explicit chunker versions, and backend-independent staging/commit semantics.
- This model is a strong candidate for read-only/versioned distribution over object storage. It should remain separate from w9pt's live filesystem/NFS semantics.

## 14. Sources

Official Valve sources:

- [Steamworks: Uploading to Steam / SteamPipe](https://partner.steamgames.com/doc/sdk/uploading)
- [Steamworks: Builds and manifests](https://partner.steamgames.com/doc/store/application/builds)
- [Steamworks: Local Content Server](https://partner.steamgames.com/doc/sdk/uploading/local_content_server)
- [Steamworks SDK contents](https://partner.steamgames.com/doc/sdk)
- [Valve Steam Blog: Download Better, Stronger, Faster](https://store.steampowered.com/news/5856/)
- [ValveSoftware public GitHub organization](https://github.com/ValveSoftware)

Community source implementations:

- [SteamRE/SteamKit (LGPL-2.1)](https://github.com/SteamRE/SteamKit)
- [SteamKit2 manifest parser](https://github.com/SteamRE/SteamKit/blob/master/SteamKit2/SteamKit2/Types/Manifest.cs)
- [SteamRE/DepotDownloader (GPL-2.0)](https://github.com/SteamRE/DepotDownloader)
- [DepotDownloader update/download pipeline](https://github.com/SteamRE/DepotDownloader/blob/master/DepotDownloader/ContentDownloader.cs)
- [ValvePython/steam (MIT)](https://github.com/ValvePython/steam)
- [TEK Steam Client (GPL-3.0)](https://github.com/teknology-hub/tek-steamclient)
- [HLLib legacy GCF parser (LGPL)](https://github.com/mikkokko/HLLib)

Comparable open designs:

- [systemd/casync](https://github.com/systemd/casync)
- [zchunk](https://github.com/zchunk/zchunk)
- [restic/chunker](https://github.com/restic/chunker)
