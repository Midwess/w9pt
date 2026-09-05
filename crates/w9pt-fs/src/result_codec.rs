//! Canonical fingerprints and bounded versioned terminal-result codecs.

use core::fmt;

use w9pt::{
    Qid,
    filesystem::{
        CreateResult, FilesystemOperation, FilesystemRequest, FilesystemResult, NodeResult,
        ObjectHandle, OpenHandle, OpenResult, XattrHandle,
    },
    protocol::QidType,
};
use w9pt_fs_state::{
    BoundedValueError, MutationResult, MutationResultKind, RequestFingerprint, ResultFormatVersion,
    StateLimits,
};

use crate::{EngineLimitError, EngineLimits, ExecutionContext, ExecutionContextError, ExportGrant};

const RESULT_FORMAT_V1: u16 = 1;
const RESULT_RELEASED: u16 = 1;
const RESULT_OPENED: u16 = 2;
const RESULT_CREATED: u16 = 3;
const RESULT_DIRECTORY_CREATED: u16 = 4;
const RESULT_SYMLINK_CREATED: u16 = 5;
const RESULT_WRITTEN: u16 = 6;
const RESULT_ATTRIBUTES_SET: u16 = 7;
const RESULT_RENAMED_AT: u16 = 8;
const RESULT_UNLINKED: u16 = 9;
const RESULT_LINKED: u16 = 10;

/// Failure constructing a complete bounded semantic mutation fingerprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FingerprintError {
    /// Required mutation identity, fence, or frozen time was absent.
    Context(ExecutionContextError),
    /// The operation is read-only or belongs to a deferred slice.
    NotFirstSliceMutation,
    /// Canonical input exceeded the configured fingerprint bound.
    Limit(EngineLimitError),
    /// Canonical length arithmetic overflowed.
    Arithmetic,
}

impl fmt::Display for FingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "mutation fingerprint failed: {self:?}")
    }
}

impl std::error::Error for FingerprintError {}

/// Constructs a canonical fingerprint of the complete pre-allocation mutation intent.
///
/// Allocation outputs are deliberately excluded. The engine must be able to reconstruct this
/// value and probe the ledger before consulting state high-water marks or allocating identities.
/// Exact allocated handles, QIDs, cookies, and counts are instead retained in the terminal result.
pub fn mutation_fingerprint(
    request: &FilesystemRequest,
    execution: &ExecutionContext,
    grant: &ExportGrant,
    limits: EngineLimits,
) -> Result<RequestFingerprint, FingerprintError> {
    let mutation_id = execution.mutation_id.ok_or(FingerprintError::Context(
        ExecutionContextError::MissingMutationId,
    ))?;
    let fence = execution.fence.ok_or(FingerprintError::Context(
        ExecutionContextError::MissingWriterFence,
    ))?;
    let timestamp = execution.timestamp.ok_or(FingerprintError::Context(
        ExecutionContextError::MissingTimestamp,
    ))?;
    let mut writer = FingerprintWriter::new(limits);
    writer.bytes(b"w9pt-fs-mutation-v1\0")?;
    writer.u64(request.context.session_id.get())?;
    writer.sized(request.context.principal.as_str().as_bytes())?;
    writer.sized(request.context.export.as_str().as_bytes())?;
    writer.bytes(execution.client_incarnation.as_bytes())?;
    writer.bytes(mutation_id.as_bytes())?;
    writer.u64(execution.retention.horizon())?;
    writer.bytes(fence.scope.as_bytes())?;
    writer.bytes(fence.holder.as_bytes())?;
    writer.bytes(fence.lease_id.as_bytes())?;
    writer.u64(fence.fencing_token.get())?;
    writer.i64(timestamp.seconds())?;
    writer.u32(timestamp.nanoseconds())?;
    writer.bytes(grant.filesystem_id().as_bytes())?;
    writer.bytes(grant.root_inode_id().as_bytes())?;
    writer.sized(grant.principal().as_bytes())?;
    writer.sized(grant.primary_group().as_bytes())?;
    writer.u64(
        u64::try_from(grant.supplementary_groups().len())
            .map_err(|_| FingerprintError::Arithmetic)?,
    )?;
    for group in grant.supplementary_groups() {
        writer.sized(group.as_bytes())?;
    }
    writer.u32(grant.numeric_uid())?;
    writer.u32(grant.numeric_gid())?;
    writer.byte(u8::from(grant.privileged()))?;
    writer.byte(u8::from(grant.read_only()))?;
    writer.u64(grant.policy_generation())?;
    writer.u128(grant.capability_ceiling().bits())?;
    encode_operation(&mut writer, &request.operation)?;
    Ok(RequestFingerprint::blake3(&writer.bytes))
}

fn encode_operation(
    writer: &mut FingerprintWriter,
    operation: &FilesystemOperation,
) -> Result<(), FingerprintError> {
    match operation {
        FilesystemOperation::Release {
            object,
            open,
            xattr,
        } => {
            writer.byte(1)?;
            writer.object(*object)?;
            writer.optional_open(*open)?;
            writer.optional_xattr(*xattr)
        }
        FilesystemOperation::Open { object, flags } => {
            writer.byte(2)?;
            writer.object(*object)?;
            writer.u32(flags.bits())
        }
        FilesystemOperation::Create {
            directory,
            name,
            flags,
            mode,
            gid,
        } => {
            writer.byte(3)?;
            writer.object(*directory)?;
            writer.sized(name.as_bytes())?;
            writer.u32(flags.bits())?;
            writer.u32(*mode)?;
            writer.u32(*gid)
        }
        FilesystemOperation::Mkdir {
            directory,
            name,
            mode,
            gid,
        } => {
            writer.byte(4)?;
            writer.object(*directory)?;
            writer.sized(name.as_bytes())?;
            writer.u32(*mode)?;
            writer.u32(*gid)
        }
        FilesystemOperation::Symlink {
            directory,
            name,
            target,
            gid,
        } => {
            writer.byte(5)?;
            writer.object(*directory)?;
            writer.sized(name.as_bytes())?;
            writer.sized(target.as_bytes())?;
            writer.u32(*gid)
        }
        FilesystemOperation::Write { open, offset, data } => {
            writer.byte(6)?;
            writer.open(*open)?;
            writer.u64(*offset)?;
            writer.sized(data)
        }
        FilesystemOperation::Setattr { object, attributes } => {
            writer.byte(7)?;
            writer.object(*object)?;
            writer.u32(attributes.valid.bits())?;
            writer.u32(attributes.mode)?;
            writer.u32(attributes.uid)?;
            writer.u32(attributes.gid)?;
            writer.u64(attributes.size)?;
            writer.u64(attributes.accessed.seconds)?;
            writer.u64(attributes.accessed.nanoseconds)?;
            writer.u64(attributes.modified.seconds)?;
            writer.u64(attributes.modified.nanoseconds)
        }
        FilesystemOperation::RenameAt {
            old_directory,
            old_name,
            new_directory,
            new_name,
        } => {
            writer.byte(8)?;
            writer.object(*old_directory)?;
            writer.sized(old_name.as_bytes())?;
            writer.object(*new_directory)?;
            writer.sized(new_name.as_bytes())
        }
        FilesystemOperation::UnlinkAt {
            directory,
            name,
            flags,
        } => {
            writer.byte(9)?;
            writer.object(*directory)?;
            writer.sized(name.as_bytes())?;
            writer.u32(flags.bits())
        }
        FilesystemOperation::Link {
            directory,
            target,
            name,
        } => {
            writer.byte(10)?;
            writer.object(*directory)?;
            writer.object(*target)?;
            writer.sized(name.as_bytes())
        }
        _ => Err(FingerprintError::NotFirstSliceMutation),
    }
}

struct FingerprintWriter {
    bytes: Vec<u8>,
    limits: EngineLimits,
}

impl FingerprintWriter {
    fn new(limits: EngineLimits) -> Self {
        Self {
            bytes: Vec::new(),
            limits,
        }
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), FingerprintError> {
        let length = self
            .bytes
            .len()
            .checked_add(value.len())
            .ok_or(FingerprintError::Arithmetic)?;
        self.limits
            .check_fingerprint_bytes(length)
            .map_err(FingerprintError::Limit)?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn sized(&mut self, value: &[u8]) -> Result<(), FingerprintError> {
        self.u64(u64::try_from(value.len()).map_err(|_| FingerprintError::Arithmetic)?)?;
        self.bytes(value)
    }

    fn byte(&mut self, value: u8) -> Result<(), FingerprintError> {
        self.bytes(&[value])
    }

    fn u32(&mut self, value: u32) -> Result<(), FingerprintError> {
        self.bytes(&value.to_le_bytes())
    }

    fn u64(&mut self, value: u64) -> Result<(), FingerprintError> {
        self.bytes(&value.to_le_bytes())
    }

    fn i64(&mut self, value: i64) -> Result<(), FingerprintError> {
        self.bytes(&value.to_le_bytes())
    }

    fn u128(&mut self, value: u128) -> Result<(), FingerprintError> {
        self.bytes(&value.to_le_bytes())
    }

    fn object(&mut self, value: ObjectHandle) -> Result<(), FingerprintError> {
        self.bytes(&value.get().to_be_bytes())
    }

    fn open(&mut self, value: OpenHandle) -> Result<(), FingerprintError> {
        self.bytes(&value.get().to_be_bytes())
    }

    fn optional_open(&mut self, value: Option<OpenHandle>) -> Result<(), FingerprintError> {
        match value {
            Some(value) => {
                self.byte(1)?;
                self.open(value)
            }
            None => self.byte(0),
        }
    }

    fn optional_xattr(&mut self, value: Option<XattrHandle>) -> Result<(), FingerprintError> {
        match value {
            Some(value) => {
                self.byte(1)?;
                self.bytes(&value.get().to_be_bytes())
            }
            None => self.byte(0),
        }
    }
}

/// Failure encoding or decoding a retained terminal mutation result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResultCodecError {
    /// Successful result does not belong to the implemented mutating slice.
    UnsupportedResult,
    /// Retained result uses an unknown kind tag.
    UnknownKind(u16),
    /// Retained result uses an unknown codec version.
    UnknownVersion(u16),
    /// Payload length or trailing bytes are not canonical for its kind.
    InvalidLength,
    /// QID path is zero or QID type is outside the first slice.
    InvalidQid,
    /// Engine codec byte bound was exceeded.
    Limit(EngineLimitError),
    /// State mutation-result construction rejected the value.
    State(BoundedValueError),
}

impl fmt::Display for ResultCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "mutation result codec failed: {self:?}")
    }
}

impl std::error::Error for ResultCodecError {}

/// Encodes one successful first-slice mutation result for authoritative replay.
pub fn encode_mutation_result(
    result: &FilesystemResult,
    engine_limits: EngineLimits,
    state_limits: StateLimits,
) -> Result<MutationResult, ResultCodecError> {
    let (kind, bytes) = match result {
        FilesystemResult::Released => (RESULT_RELEASED, Vec::new()),
        FilesystemResult::Opened(result) => (RESULT_OPENED, encode_opened(*result)),
        FilesystemResult::Created(result) => (RESULT_CREATED, encode_created(*result)),
        FilesystemResult::DirectoryCreated(result) => {
            (RESULT_DIRECTORY_CREATED, encode_node(*result))
        }
        FilesystemResult::SymlinkCreated(result) => (RESULT_SYMLINK_CREATED, encode_node(*result)),
        FilesystemResult::Written(count) => (RESULT_WRITTEN, count.to_le_bytes().to_vec()),
        FilesystemResult::AttributesSet => (RESULT_ATTRIBUTES_SET, Vec::new()),
        FilesystemResult::RenamedAt => (RESULT_RENAMED_AT, Vec::new()),
        FilesystemResult::Unlinked => (RESULT_UNLINKED, Vec::new()),
        FilesystemResult::Linked => (RESULT_LINKED, Vec::new()),
        _ => return Err(ResultCodecError::UnsupportedResult),
    };
    engine_limits
        .check_result_bytes(bytes.len())
        .map_err(ResultCodecError::Limit)?;
    MutationResult::new(
        MutationResultKind::new(kind).map_err(ResultCodecError::State)?,
        ResultFormatVersion::new(RESULT_FORMAT_V1).map_err(ResultCodecError::State)?,
        bytes,
        state_limits,
    )
    .map_err(ResultCodecError::State)
}

/// Decodes one authoritative retained result into its exact client-safe success value.
pub fn decode_mutation_result(
    retained: &MutationResult,
    limits: EngineLimits,
) -> Result<FilesystemResult, ResultCodecError> {
    if retained.format().get() != RESULT_FORMAT_V1 {
        return Err(ResultCodecError::UnknownVersion(retained.format().get()));
    }
    limits
        .check_result_bytes(retained.bytes().len())
        .map_err(ResultCodecError::Limit)?;
    let bytes = retained.bytes();
    match retained.kind().get() {
        RESULT_RELEASED => decode_unit(bytes, FilesystemResult::Released),
        RESULT_OPENED => decode_opened(bytes).map(FilesystemResult::Opened),
        RESULT_CREATED => decode_created(bytes).map(FilesystemResult::Created),
        RESULT_DIRECTORY_CREATED => decode_node(bytes).map(FilesystemResult::DirectoryCreated),
        RESULT_SYMLINK_CREATED => decode_node(bytes).map(FilesystemResult::SymlinkCreated),
        RESULT_WRITTEN if bytes.len() == 4 => Ok(FilesystemResult::Written(u32::from_le_bytes(
            bytes.try_into().expect("length was checked"),
        ))),
        RESULT_WRITTEN => Err(ResultCodecError::InvalidLength),
        RESULT_ATTRIBUTES_SET => decode_unit(bytes, FilesystemResult::AttributesSet),
        RESULT_RENAMED_AT => decode_unit(bytes, FilesystemResult::RenamedAt),
        RESULT_UNLINKED => decode_unit(bytes, FilesystemResult::Unlinked),
        RESULT_LINKED => decode_unit(bytes, FilesystemResult::Linked),
        other => Err(ResultCodecError::UnknownKind(other)),
    }
}

fn encode_qid(qid: Qid, bytes: &mut Vec<u8>) {
    bytes.push(qid.ty.bits());
    bytes.extend_from_slice(&qid.version.to_le_bytes());
    bytes.extend_from_slice(&qid.path.to_le_bytes());
}

fn encode_opened(result: OpenResult) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(33);
    encode_qid(result.qid, &mut bytes);
    bytes.extend_from_slice(&result.open.get().to_be_bytes());
    bytes.extend_from_slice(&result.io_unit.to_le_bytes());
    bytes
}

fn encode_created(result: CreateResult) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(49);
    bytes.extend_from_slice(&result.object.get().to_be_bytes());
    encode_qid(result.qid, &mut bytes);
    bytes.extend_from_slice(&result.open.get().to_be_bytes());
    bytes.extend_from_slice(&result.io_unit.to_le_bytes());
    bytes
}

fn encode_node(result: NodeResult) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(29);
    bytes.extend_from_slice(&result.object.get().to_be_bytes());
    encode_qid(result.qid, &mut bytes);
    bytes
}

fn decode_unit(
    bytes: &[u8],
    result: FilesystemResult,
) -> Result<FilesystemResult, ResultCodecError> {
    if bytes.is_empty() {
        Ok(result)
    } else {
        Err(ResultCodecError::InvalidLength)
    }
}

fn decode_qid(bytes: &[u8]) -> Result<Qid, ResultCodecError> {
    if bytes.len() != 13 {
        return Err(ResultCodecError::InvalidLength);
    }
    let ty = bytes[0];
    if !matches!(ty, 0 | 0x02 | 0x80) {
        return Err(ResultCodecError::InvalidQid);
    }
    let version = u32::from_le_bytes(bytes[1..5].try_into().expect("fixed slice"));
    let path = u64::from_le_bytes(bytes[5..13].try_into().expect("fixed slice"));
    if path == 0 {
        return Err(ResultCodecError::InvalidQid);
    }
    Ok(Qid::new(QidType::from_bits(ty), version, path))
}

fn decode_opened(bytes: &[u8]) -> Result<OpenResult, ResultCodecError> {
    if bytes.len() != 33 {
        return Err(ResultCodecError::InvalidLength);
    }
    Ok(OpenResult {
        qid: decode_qid(&bytes[..13])?,
        open: OpenHandle::new(u128::from_be_bytes(
            bytes[13..29].try_into().expect("fixed slice"),
        )),
        io_unit: u32::from_le_bytes(bytes[29..33].try_into().expect("fixed slice")),
    })
}

fn decode_created(bytes: &[u8]) -> Result<CreateResult, ResultCodecError> {
    if bytes.len() != 49 {
        return Err(ResultCodecError::InvalidLength);
    }
    Ok(CreateResult {
        object: ObjectHandle::new(u128::from_be_bytes(
            bytes[..16].try_into().expect("fixed slice"),
        )),
        qid: decode_qid(&bytes[16..29])?,
        open: OpenHandle::new(u128::from_be_bytes(
            bytes[29..45].try_into().expect("fixed slice"),
        )),
        io_unit: u32::from_le_bytes(bytes[45..49].try_into().expect("fixed slice")),
    })
}

fn decode_node(bytes: &[u8]) -> Result<NodeResult, ResultCodecError> {
    if bytes.len() != 29 {
        return Err(ResultCodecError::InvalidLength);
    }
    Ok(NodeResult {
        object: ObjectHandle::new(u128::from_be_bytes(
            bytes[..16].try_into().expect("fixed slice"),
        )),
        qid: decode_qid(&bytes[16..])?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use w9pt::{
        SessionId,
        filesystem::{ExportId, PrincipalId as WirePrincipal, RequestContext},
    };
    use w9pt_fs_state::{
        ClientIncarnationId, FencingToken, FilesystemId, GroupId, InodeId, LeaseId,
        MutationRetention, PrincipalId, UnixTimestamp, WriterFence, WriterIncarnationId,
        WriterScopeId,
    };
    use w9pt_fs_storage::MutationId;

    fn round_trip(result: FilesystemResult) {
        let encoded =
            encode_mutation_result(&result, EngineLimits::default(), StateLimits::default())
                .unwrap();
        assert_eq!(
            decode_mutation_result(&encoded, EngineLimits::default()).unwrap(),
            result
        );
    }

    #[test]
    fn every_first_slice_mutation_result_round_trips() {
        let qid = Qid::new(QidType::DIRECTORY, 2, 3);
        for result in [
            FilesystemResult::Released,
            FilesystemResult::Opened(OpenResult {
                qid,
                open: OpenHandle::new(4),
                io_unit: 5,
            }),
            FilesystemResult::Created(CreateResult {
                object: ObjectHandle::new(1),
                qid,
                open: OpenHandle::new(4),
                io_unit: 5,
            }),
            FilesystemResult::DirectoryCreated(NodeResult {
                object: ObjectHandle::new(1),
                qid,
            }),
            FilesystemResult::SymlinkCreated(NodeResult {
                object: ObjectHandle::new(1),
                qid,
            }),
            FilesystemResult::Written(7),
            FilesystemResult::AttributesSet,
            FilesystemResult::RenamedAt,
            FilesystemResult::Unlinked,
            FilesystemResult::Linked,
        ] {
            round_trip(result);
        }
    }

    #[test]
    fn every_first_slice_result_has_an_independent_golden_fixture() {
        #[rustfmt::skip]
        const OPENED: &[u8] = &[
            0x80, 2, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4,
            5, 0, 0, 0,
        ];
        #[rustfmt::skip]
        const CREATED: &[u8] = &[
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            0x80, 2, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4,
            5, 0, 0, 0,
        ];
        #[rustfmt::skip]
        const NODE: &[u8] = &[
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            0x80, 2, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0,
        ];
        let qid = Qid::new(QidType::DIRECTORY, 2, 3);
        let fixtures: Vec<(FilesystemResult, u16, &[u8])> = vec![
            (FilesystemResult::Released, 1, &[]),
            (
                FilesystemResult::Opened(OpenResult {
                    qid,
                    open: OpenHandle::new(4),
                    io_unit: 5,
                }),
                2,
                OPENED,
            ),
            (
                FilesystemResult::Created(CreateResult {
                    object: ObjectHandle::new(1),
                    qid,
                    open: OpenHandle::new(4),
                    io_unit: 5,
                }),
                3,
                CREATED,
            ),
            (
                FilesystemResult::DirectoryCreated(NodeResult {
                    object: ObjectHandle::new(1),
                    qid,
                }),
                4,
                NODE,
            ),
            (
                FilesystemResult::SymlinkCreated(NodeResult {
                    object: ObjectHandle::new(1),
                    qid,
                }),
                5,
                NODE,
            ),
            (FilesystemResult::Written(7), 6, &[7, 0, 0, 0]),
            (FilesystemResult::AttributesSet, 7, &[]),
            (FilesystemResult::RenamedAt, 8, &[]),
            (FilesystemResult::Unlinked, 9, &[]),
            (FilesystemResult::Linked, 10, &[]),
        ];
        for (result, kind, bytes) in fixtures {
            let encoded =
                encode_mutation_result(&result, EngineLimits::default(), StateLimits::default())
                    .unwrap();
            assert_eq!(encoded.kind().get(), kind);
            assert_eq!(encoded.format().get(), 1);
            assert_eq!(encoded.bytes(), bytes);

            let fixture = MutationResult::new(
                MutationResultKind::new(kind).unwrap(),
                ResultFormatVersion::new(1).unwrap(),
                bytes.to_vec(),
                StateLimits::default(),
            )
            .unwrap();
            assert_eq!(
                decode_mutation_result(&fixture, EngineLimits::default()).unwrap(),
                result
            );
        }
    }

    #[test]
    fn decoder_rejects_unknown_versions_lengths_qids_and_trailing_data() {
        let state_limits = StateLimits::default();
        let bad_version = MutationResult::new(
            MutationResultKind::new(RESULT_RELEASED).unwrap(),
            ResultFormatVersion::new(2).unwrap(),
            Vec::new(),
            state_limits,
        )
        .unwrap();
        assert!(matches!(
            decode_mutation_result(&bad_version, EngineLimits::default()),
            Err(ResultCodecError::UnknownVersion(2))
        ));
        let trailing = MutationResult::new(
            MutationResultKind::new(RESULT_LINKED).unwrap(),
            ResultFormatVersion::new(1).unwrap(),
            vec![0],
            state_limits,
        )
        .unwrap();
        assert_eq!(
            decode_mutation_result(&trailing, EngineLimits::default()),
            Err(ResultCodecError::InvalidLength)
        );
        let mut opened = encode_opened(OpenResult {
            qid: Qid::new(QidType::FILE, 0, 1),
            open: OpenHandle::new(1),
            io_unit: 0,
        });
        opened[5..13].fill(0);
        let invalid_qid = MutationResult::new(
            MutationResultKind::new(RESULT_OPENED).unwrap(),
            ResultFormatVersion::new(1).unwrap(),
            opened,
            state_limits,
        )
        .unwrap();
        assert_eq!(
            decode_mutation_result(&invalid_qid, EngineLimits::default()),
            Err(ResultCodecError::InvalidQid)
        );
    }

    #[test]
    fn pre_allocation_fingerprint_has_an_independent_golden_vector() {
        let state_limits = StateLimits::default();
        let filesystem_id = FilesystemId::from_u128(1);
        let request = FilesystemRequest::new(
            RequestContext::new(
                SessionId::new(2),
                WirePrincipal::new("wire-principal"),
                ExportId::new("export"),
            ),
            FilesystemOperation::Write {
                open: OpenHandle::new(3),
                offset: 4,
                data: b"data".to_vec(),
            },
        );
        let mutation_id = MutationId::from_u128(5);
        let execution = ExecutionContext::new(
            ClientIncarnationId::from_u128(6),
            Some(mutation_id),
            MutationRetention::new(7),
            Some(WriterFence::new(
                WriterScopeId::from_u128(8),
                WriterIncarnationId::from_u128(9),
                LeaseId::from_u128(10),
                FencingToken::new(11).unwrap(),
            )),
            Some(UnixTimestamp::new(12, 13).unwrap()),
        );
        let grant = ExportGrant::new(
            filesystem_id,
            InodeId::from_u128(14),
            PrincipalId::new(b"principal".to_vec(), state_limits).unwrap(),
            GroupId::new(b"group".to_vec(), state_limits).unwrap(),
            Vec::new(),
            15,
            16,
            false,
            false,
            17,
            w9pt::filesystem::CapabilitySet::NONE,
            EngineLimits::default(),
        )
        .unwrap();
        let fingerprint =
            mutation_fingerprint(&request, &execution, &grant, EngineLimits::default()).unwrap();
        assert_eq!(
            fingerprint.as_bytes(),
            &[
                0xcb, 0x0c, 0x19, 0xcb, 0xd1, 0x06, 0xe1, 0x1e, 0x69, 0x31, 0x11, 0x45, 0x07, 0xde,
                0xae, 0xd8, 0x55, 0xfc, 0xe2, 0xe0, 0x12, 0xee, 0xae, 0xbd, 0x19, 0x35, 0x91, 0x81,
                0xbd, 0xcb, 0x4b, 0xc6,
            ]
        );
    }
}
