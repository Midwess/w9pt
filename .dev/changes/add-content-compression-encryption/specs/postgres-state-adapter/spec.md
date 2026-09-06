# Delta for PostgreSQL State Adapter

## ADDED Requirements

### Requirement: PostgreSQL File Content Metadata

The adapter SHALL persist generic ContentMetadata in a normalized permanent
logged `public.w9pt_fs_state_content_metadata` table with stable filesystem,
content FileId, owner inode/context identity and bounded opaque policy/wrapped
key fields. It SHALL store no plaintext KEK/DEK and execute no crypto/codec logic.

#### Scenario: Current schema is created

- WHEN the code-first schema is initialized
- THEN named constraints validate metadata identities, optional shapes, byte bounds and regular-inode context association
- AND the current schema/checksum/catalog fixtures include the new relation directly without an old-schema converter

#### Scenario: Retained context outlives its inode

- WHEN the owning inode is deleted through normal lifetime rules
- THEN no foreign-key cascade removes its content metadata
- AND the row remains addressable by filesystem/content FileId

#### Scenario: Context is fetched or scanned

- WHEN the adapter reads inode/context or enumerates contexts
- THEN exact keys, consistent snapshots and bounded keyset item/byte limits apply
- AND SQL contains no representation decoding or all-files plaintext-key loading

#### Scenario: Schema and privileges are validated

- WHEN the adapter opens a deployment
- THEN table/index/constraint/checksum and table-specific privilege checks include content metadata
- AND unsupported current schema or weakened durability fails through the existing contract

### Requirement: PostgreSQL Atomic Context Lifecycle

The adapter SHALL apply the finalized context creation, publication association,
rewrap and lifetime transitions through its existing serializable ledger-first
transaction protocol, with canonical lock order and exact revision/fence checks.

#### Scenario: File and context are created

- WHEN a create transaction inserts an unpublished inode and matching context with namespace/open changes
- THEN all records and the result commit durably together
- AND a failed transaction leaves neither a new inode nor a context reservation

#### Scenario: Replay returns the original allocation

- WHEN an independent client retries an acknowledged or ambiguously committed create
- THEN the ledger/inode/context resolve the original selected metadata
- AND a fresh candidate does not replace its wrapped key

#### Scenario: Rewrap commits or conflicts

- WHEN the dedicated transition replaces wrapped bytes under the expected context revision
- THEN only the allowed envelope/revision and change/result records change atomically
- AND stale revision/fence or immutable-field changes fail without generic replacement bypass

#### Scenario: Wrapper and content publication race

- WHEN a rewrap changes context revision before an in-flight content publication
- THEN authoritative revision checks prevent stale publication
- AND retry resolves the same stable file context rather than regenerating a DEK

#### Scenario: Envelope is malformed cryptographically

- WHEN bounded opaque bytes are persisted but fail later authenticated unwrap
- THEN the storage helper reports the key/context error
- AND PostgreSQL does not attempt decryption, silently repair bytes or claim cryptographic validation

## MODIFIED Requirements

### Requirement: Normalized PostgreSQL Record Tables

The system SHALL persist finalized authoritative record families, including
content metadata, in normalized logged tables under `public` using the fixed
`w9pt_fs_state_` prefix. Adapter-private coordination rows remain distinct.

#### Scenario: Fresh code-first schema is inspected

- WHEN catalog parity validation runs
- THEN names, columns, types, defaults, keys, deferred constraints and indexes match the current schema including context retention rules
- AND no production object depends on search_path

#### Scenario: Runtime SQL addresses state

- WHEN the adapter reads or mutates a record
- THEN statements qualify relations explicitly under public
- AND no visible file path, payload or block map is used as a database object name

### Requirement: Dedicated Content and Xattr Publication

The system SHALL implement finalized content/context and xattr transitions
without storing actual file data or allowing generic publication bypasses.

#### Scenario: Prepared content is valid

- GIVEN immutable content dependencies and root are already durable
- WHEN publication validates mutation/base/file/size/generations and context/policy/key binding against current revisions
- THEN ContentRef and inode summaries/result publish atomically
- AND PostgreSQL performs no target-object or cryptographic operation

#### Scenario: Prepared content is inconsistent

- WHEN any finalized publication invariant disagrees with authoritative state
- THEN the transaction returns the documented malformed/conflict outcome
- AND old content remains selected

#### Scenario: Xattr staging is published

- WHEN PublishXattrStaging references a complete matching staging record
- THEN staging removal and xattr publication occur atomically
- AND generic changes cannot bypass dedicated validation

#### Scenario: Per-block mapping is requested

- WHEN a caller wants PostgreSQL-resident block or extent mappings
- THEN this adapter change does not provide that operation
- AND the wrapped-key metadata feature does not move S3 mapping pages into SQL
