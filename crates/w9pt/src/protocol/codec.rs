//! Checked little-endian request decoder and response encoder.

use crate::{
    error::{DecodeError, EncodeError},
    limits::Limits,
};

use super::{
    Fid, GetattrMask, HEADER_SIZE, Lock, LockFlags, LockRequest, LockType, MessageType, OpenFlags,
    Qid, Request, RequestBody, Response, ResponseBody, SetAttributes, SetattrMask, StructureError,
    Tag, Timestamp, UnlinkFlags, XattrFlags, XattrName,
};

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub const fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(DecodeError::ArithmeticOverflow)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(DecodeError::Incomplete {
                needed: count,
                available: self.remaining(),
            })?;
        self.offset = end;
        Ok(value)
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("exact checked length"),
        ))
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("exact checked length"),
        ))
    }

    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("exact checked length"),
        ))
    }

    pub fn string(&mut self, maximum: usize) -> Result<String, DecodeError> {
        let length = usize::from(self.u16()?);
        if length > maximum {
            return Err(DecodeError::StringTooLong { length, maximum });
        }
        let offset = self.offset;
        let value = core::str::from_utf8(self.take(length)?)
            .map_err(|_| DecodeError::InvalidUtf8 { offset })?;
        Ok(value.to_owned())
    }

    pub fn bytes(&mut self, count: usize) -> Result<Vec<u8>, DecodeError> {
        Ok(self.take(count)?.to_vec())
    }

    pub fn finish(self) -> Result<(), DecodeError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes {
                remaining: self.remaining(),
            })
        }
    }
}

pub(crate) struct Writer {
    bytes: Vec<u8>,
    maximum: u32,
}

impl Writer {
    pub fn frame(message_type: MessageType, tag: Tag, maximum: u32) -> Result<Self, EncodeError> {
        if maximum < HEADER_SIZE as u32 {
            return Err(EncodeError::FrameTooLarge {
                size: HEADER_SIZE,
                maximum,
            });
        }
        let mut writer = Self {
            bytes: Vec::with_capacity(64),
            maximum,
        };
        writer.bytes.extend_from_slice(&[0; 4]);
        writer.u8(message_type.to_u8())?;
        writer.u16(tag.get())?;
        Ok(writer)
    }

    fn reserve(&mut self, additional: usize) -> Result<(), EncodeError> {
        let size = self
            .bytes
            .len()
            .checked_add(additional)
            .ok_or(EncodeError::ArithmeticOverflow)?;
        if size > self.maximum as usize {
            return Err(EncodeError::FrameTooLarge {
                size,
                maximum: self.maximum,
            });
        }
        self.bytes.reserve(additional);
        Ok(())
    }

    pub fn u8(&mut self, value: u8) -> Result<(), EncodeError> {
        self.reserve(1)?;
        self.bytes.push(value);
        Ok(())
    }

    pub fn u16(&mut self, value: u16) -> Result<(), EncodeError> {
        self.extend(&value.to_le_bytes())
    }

    pub fn u32(&mut self, value: u32) -> Result<(), EncodeError> {
        self.extend(&value.to_le_bytes())
    }

    pub fn u64(&mut self, value: u64) -> Result<(), EncodeError> {
        self.extend(&value.to_le_bytes())
    }

    pub fn string(&mut self, value: &str) -> Result<(), EncodeError> {
        let length = u16::try_from(value.len()).map_err(|_| EncodeError::StringTooLong {
            length: value.len(),
            maximum: u16::MAX as usize,
        })?;
        self.u16(length)?;
        self.extend(value.as_bytes())
    }

    pub fn qid(&mut self, value: Qid) -> Result<(), EncodeError> {
        self.u8(value.ty.bits())?;
        self.u32(value.version)?;
        self.u64(value.path)
    }

    pub fn extend(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        self.reserve(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    pub fn finish(mut self) -> Result<Vec<u8>, EncodeError> {
        let size = u32::try_from(self.bytes.len()).map_err(|_| EncodeError::ArithmeticOverflow)?;
        self.bytes[..4].copy_from_slice(&size.to_le_bytes());
        Ok(self.bytes)
    }
}

/// Validates a complete frame's outer size and configured maximum.
pub fn validate_frame(frame: &[u8], maximum: u32) -> Result<(), DecodeError> {
    if frame.len() < 4 {
        return Err(DecodeError::Incomplete {
            needed: 4,
            available: frame.len(),
        });
    }
    let declared = u32::from_le_bytes(frame[..4].try_into().expect("checked length"));
    if declared < HEADER_SIZE as u32 {
        return Err(DecodeError::FrameTooSmall { size: declared });
    }
    if declared > maximum {
        return Err(DecodeError::FrameTooLarge {
            size: declared,
            maximum,
        });
    }
    if declared as usize != frame.len() {
        return Err(DecodeError::LengthMismatch {
            declared,
            actual: frame.len(),
        });
    }
    Ok(())
}

/// Decodes one complete request frame.
pub fn decode_request(frame: &[u8], limits: &Limits, maximum: u32) -> Result<Request, DecodeError> {
    validate_frame(frame, maximum)?;
    let mut reader = Reader::new(frame);
    let _declared_size = reader.u32()?;
    let raw_type = reader.u8()?;
    let message_type = MessageType::try_from(raw_type).map_err(DecodeError::UnknownMessageType)?;
    if !message_type.is_request() {
        return Err(DecodeError::UnexpectedResponse(raw_type));
    }
    let tag = Tag::new(reader.u16()?);
    let string_limit = limits.max_string_bytes;

    let body = match message_type {
        MessageType::Tversion => RequestBody::Version {
            msize: reader.u32()?,
            version: reader.string(string_limit)?,
        },
        MessageType::Tauth => RequestBody::Auth {
            afid: Fid::new(reader.u32()?),
            uname: reader.string(string_limit)?,
            aname: reader.string(string_limit)?,
            n_uname: reader.u32()?,
        },
        MessageType::Tattach => RequestBody::Attach {
            fid: Fid::new(reader.u32()?),
            afid: Fid::new(reader.u32()?),
            uname: reader.string(string_limit)?,
            aname: reader.string(string_limit)?,
            n_uname: reader.u32()?,
        },
        MessageType::Tflush => RequestBody::Flush {
            old_tag: Tag::new(reader.u16()?),
        },
        MessageType::Twalk => {
            let fid = Fid::new(reader.u32()?);
            let new_fid = Fid::new(reader.u32()?);
            let count = usize::from(reader.u16()?);
            if count > limits.max_walk_elements {
                return Err(DecodeError::CountTooLarge {
                    kind: "walk elements",
                    count,
                    maximum: limits.max_walk_elements,
                });
            }
            let mut names = Vec::with_capacity(count);
            for _ in 0..count {
                let name = reader.string(string_limit)?;
                validate_component_name(&name)?;
                names.push(name);
            }
            RequestBody::Walk {
                fid,
                new_fid,
                names,
            }
        }
        MessageType::Tclunk => RequestBody::Clunk {
            fid: Fid::new(reader.u32()?),
        },
        MessageType::Tlopen => RequestBody::Lopen {
            fid: Fid::new(reader.u32()?),
            flags: OpenFlags::from_bits(reader.u32()?),
        },
        MessageType::Tlcreate => {
            let fid = Fid::new(reader.u32()?);
            let name = reader.string(string_limit)?;
            validate_component_name(&name)?;
            RequestBody::Lcreate {
                fid,
                name,
                flags: OpenFlags::from_bits(reader.u32()?),
                mode: reader.u32()?,
                gid: reader.u32()?,
            }
        }
        MessageType::Tmkdir => {
            let directory = Fid::new(reader.u32()?);
            let name = reader.string(string_limit)?;
            validate_component_name(&name)?;
            RequestBody::Mkdir {
                directory,
                name,
                mode: reader.u32()?,
                gid: reader.u32()?,
            }
        }
        MessageType::Tmknod => {
            let directory = Fid::new(reader.u32()?);
            let name = reader.string(string_limit)?;
            validate_component_name(&name)?;
            RequestBody::Mknod {
                directory,
                name,
                mode: reader.u32()?,
                major: reader.u32()?,
                minor: reader.u32()?,
                gid: reader.u32()?,
            }
        }
        MessageType::Tsymlink => {
            let directory = Fid::new(reader.u32()?);
            let name = reader.string(string_limit)?;
            validate_component_name(&name)?;
            RequestBody::Symlink {
                directory,
                name,
                target: reader.string(string_limit)?,
                gid: reader.u32()?,
            }
        }
        MessageType::Tread => RequestBody::Read {
            fid: Fid::new(reader.u32()?),
            offset: reader.u64()?,
            count: reader.u32()?,
        },
        MessageType::Twrite => {
            let fid = Fid::new(reader.u32()?);
            let offset = reader.u64()?;
            let count = reader.u32()? as usize;
            if count > limits.max_pending_write_bytes {
                return Err(DecodeError::CountTooLarge {
                    kind: "write bytes",
                    count,
                    maximum: limits.max_pending_write_bytes,
                });
            }
            RequestBody::Write {
                fid,
                offset,
                data: reader.bytes(count)?,
            }
        }
        MessageType::Treaddir => RequestBody::Readdir {
            fid: Fid::new(reader.u32()?),
            offset: reader.u64()?,
            count: reader.u32()?,
        },
        MessageType::Tfsync => {
            let fid = Fid::new(reader.u32()?);
            let data_only = match reader.u32()? {
                0 => false,
                1 => true,
                _ => return Err(DecodeError::InvalidValue("fsync datasync")),
            };
            RequestBody::Fsync { fid, data_only }
        }
        MessageType::Tstatfs => RequestBody::Statfs {
            fid: Fid::new(reader.u32()?),
        },
        MessageType::Tgetattr => {
            let fid = Fid::new(reader.u32()?);
            let mask = GetattrMask::from_bits(reader.u64()?);
            if mask.bits() & !GetattrMask::ALL.bits() != 0 {
                return Err(DecodeError::InvalidValue("getattr mask"));
            }
            RequestBody::Getattr { fid, mask }
        }
        MessageType::Tsetattr => {
            let fid = Fid::new(reader.u32()?);
            let attributes = SetAttributes {
                valid: SetattrMask::from_bits(reader.u32()?),
                mode: reader.u32()?,
                uid: reader.u32()?,
                gid: reader.u32()?,
                size: reader.u64()?,
                accessed: Timestamp {
                    seconds: reader.u64()?,
                    nanoseconds: reader.u64()?,
                },
                modified: Timestamp {
                    seconds: reader.u64()?,
                    nanoseconds: reader.u64()?,
                },
            };
            attributes.validate().map_err(map_structure_error)?;
            RequestBody::Setattr { fid, attributes }
        }
        MessageType::Treadlink => RequestBody::Readlink {
            fid: Fid::new(reader.u32()?),
        },
        MessageType::Trename => {
            let fid = Fid::new(reader.u32()?);
            let directory = Fid::new(reader.u32()?);
            let name = reader.string(string_limit)?;
            validate_component_name(&name)?;
            RequestBody::Rename {
                fid,
                directory,
                name,
            }
        }
        MessageType::Trenameat => {
            let old_directory = Fid::new(reader.u32()?);
            let old_name = reader.string(string_limit)?;
            validate_component_name(&old_name)?;
            let new_directory = Fid::new(reader.u32()?);
            let new_name = reader.string(string_limit)?;
            validate_component_name(&new_name)?;
            RequestBody::RenameAt {
                old_directory,
                old_name,
                new_directory,
                new_name,
            }
        }
        MessageType::Tremove => RequestBody::Remove {
            fid: Fid::new(reader.u32()?),
        },
        MessageType::Tunlinkat => {
            let directory = Fid::new(reader.u32()?);
            let name = reader.string(string_limit)?;
            validate_component_name(&name)?;
            let flags = UnlinkFlags::from_bits(reader.u32()?);
            if flags.bits() & !UnlinkFlags::REMOVE_DIR.bits() != 0 {
                return Err(DecodeError::InvalidValue("unlink flags"));
            }
            RequestBody::UnlinkAt {
                directory,
                name,
                flags,
            }
        }
        MessageType::Tlink => {
            let directory = Fid::new(reader.u32()?);
            let target = Fid::new(reader.u32()?);
            let name = reader.string(string_limit)?;
            validate_component_name(&name)?;
            RequestBody::Link {
                directory,
                target,
                name,
            }
        }
        MessageType::Txattrwalk => RequestBody::XattrWalk {
            fid: Fid::new(reader.u32()?),
            new_fid: Fid::new(reader.u32()?),
            name: reader.string(string_limit)?,
        },
        MessageType::Txattrcreate => {
            let fid = Fid::new(reader.u32()?);
            let name = reader.string(string_limit)?;
            XattrName::new(name.clone()).map_err(map_structure_error)?;
            let size = reader.u64()?;
            let flags = XattrFlags::from_bits(reader.u32()?);
            XattrName::validate_flags(flags).map_err(map_structure_error)?;
            RequestBody::XattrCreate {
                fid,
                name,
                size,
                flags,
            }
        }
        MessageType::Tlock => {
            let fid = Fid::new(reader.u32()?);
            let ty = LockType::try_from(reader.u8()?)
                .map_err(|_| DecodeError::InvalidValue("lock type"))?;
            let flags = LockFlags::from_bits(reader.u32()?);
            let lock = LockRequest {
                lock: Lock {
                    ty,
                    start: reader.u64()?,
                    length: reader.u64()?,
                    process_id: reader.u32()?,
                    client_id: reader.string(string_limit)?,
                },
                flags,
            };
            lock.validate().map_err(map_structure_error)?;
            RequestBody::Lock { fid, lock }
        }
        MessageType::Tgetlock => RequestBody::Getlock {
            fid: Fid::new(reader.u32()?),
            lock: Lock {
                ty: LockType::try_from(reader.u8()?)
                    .map_err(|_| DecodeError::InvalidValue("lock type"))?,
                start: reader.u64()?,
                length: reader.u64()?,
                process_id: reader.u32()?,
                client_id: reader.string(string_limit)?,
            },
        },
        _ => return Err(DecodeError::UnexpectedResponse(raw_type)),
    };
    reader.finish()?;
    Ok(Request { tag, body })
}

fn validate_component_name(name: &str) -> Result<(), DecodeError> {
    if name.is_empty() || name.as_bytes().contains(&b'/') || name.as_bytes().contains(&0) {
        return Err(DecodeError::InvalidValue("path component"));
    }
    Ok(())
}

const fn map_structure_error(error: StructureError) -> DecodeError {
    match error {
        StructureError::InvalidNanoseconds => DecodeError::InvalidValue("nanoseconds"),
        StructureError::UnknownMaskBits => DecodeError::InvalidValue("mask bits"),
        StructureError::InvalidComponentName => DecodeError::InvalidValue("path component"),
        StructureError::InvalidXattrName => DecodeError::InvalidValue("xattr name"),
        StructureError::ConflictingXattrFlags => DecodeError::InvalidValue("xattr flags"),
        StructureError::UnknownLockFlags => DecodeError::InvalidValue("lock flags"),
    }
}

/// Encodes one typed response under the active `msize`.
pub fn encode_response(response: &Response, maximum: u32) -> Result<Vec<u8>, EncodeError> {
    let mut writer = Writer::frame(response.message_type(), response.tag, maximum)?;
    match &response.body {
        ResponseBody::Version { msize, version } => {
            writer.u32(*msize)?;
            writer.string(version)?;
        }
        ResponseBody::Auth { qid }
        | ResponseBody::Attach { qid }
        | ResponseBody::Mkdir { qid }
        | ResponseBody::Mknod { qid }
        | ResponseBody::Symlink { qid } => writer.qid(*qid)?,
        ResponseBody::Flush
        | ResponseBody::Clunk
        | ResponseBody::Fsync
        | ResponseBody::Setattr
        | ResponseBody::Rename
        | ResponseBody::RenameAt
        | ResponseBody::Remove
        | ResponseBody::UnlinkAt
        | ResponseBody::Link
        | ResponseBody::XattrCreate => {}
        ResponseBody::Walk { qids } => {
            let count = u16::try_from(qids.len()).map_err(|_| EncodeError::CountTooLarge {
                kind: "walk qids",
                count: qids.len(),
                maximum: u16::MAX as usize,
            })?;
            writer.u16(count)?;
            for qid in qids {
                writer.qid(*qid)?;
            }
        }
        ResponseBody::Lopen { qid, io_unit } | ResponseBody::Lcreate { qid, io_unit } => {
            writer.qid(*qid)?;
            writer.u32(*io_unit)?;
        }
        ResponseBody::Read { data } | ResponseBody::Readdir { data } => {
            let count = u32::try_from(data.len()).map_err(|_| EncodeError::CountTooLarge {
                kind: "response data",
                count: data.len(),
                maximum: u32::MAX as usize,
            })?;
            writer.u32(count)?;
            writer.extend(data)?;
        }
        ResponseBody::Write { count } => writer.u32(*count)?,
        ResponseBody::Statfs(statfs) => {
            writer.u32(statfs.ty)?;
            writer.u32(statfs.block_size)?;
            writer.u64(statfs.blocks)?;
            writer.u64(statfs.blocks_free)?;
            writer.u64(statfs.blocks_available)?;
            writer.u64(statfs.files)?;
            writer.u64(statfs.files_free)?;
            writer.u64(statfs.filesystem_id)?;
            writer.u32(statfs.max_name_length)?;
        }
        ResponseBody::Getattr(attributes) => {
            attributes.validate().map_err(map_structure_encode_error)?;
            writer.u64(attributes.valid.bits())?;
            writer.qid(attributes.qid)?;
            writer.u32(attributes.mode)?;
            writer.u32(attributes.uid)?;
            writer.u32(attributes.gid)?;
            writer.u64(attributes.link_count)?;
            writer.u64(attributes.device)?;
            writer.u64(attributes.size)?;
            writer.u64(attributes.block_size)?;
            writer.u64(attributes.blocks)?;
            write_timestamp(&mut writer, attributes.accessed)?;
            write_timestamp(&mut writer, attributes.modified)?;
            write_timestamp(&mut writer, attributes.changed)?;
            write_timestamp(&mut writer, attributes.created)?;
            writer.u64(attributes.generation)?;
            writer.u64(attributes.data_version)?;
        }
        ResponseBody::Readlink { target } => writer.string(target)?,
        ResponseBody::XattrWalk { size } => writer.u64(*size)?,
        ResponseBody::Lock { status } => writer.u8(*status as u8)?,
        ResponseBody::Getlock { lock } => write_lock(&mut writer, lock)?,
        ResponseBody::Lerror(error) => writer.u32(error.0.get())?,
    }
    writer.finish()
}

/// Encodes complete directory records up to `maximum_data_bytes`.
pub fn encode_directory_entries(
    entries: &[super::DirectoryEntry],
    maximum_data_bytes: usize,
    maximum_string_bytes: usize,
) -> Result<Vec<u8>, EncodeError> {
    let mut bytes = Vec::with_capacity(maximum_data_bytes.min(4096));
    for entry in entries {
        entry.validate().map_err(map_structure_encode_error)?;
        if entry.name.len() > maximum_string_bytes {
            return Err(EncodeError::StringTooLong {
                length: entry.name.len(),
                maximum: maximum_string_bytes,
            });
        }
        let name_length =
            u16::try_from(entry.name.len()).map_err(|_| EncodeError::StringTooLong {
                length: entry.name.len(),
                maximum: u16::MAX as usize,
            })?;
        let record_length = 13usize
            .checked_add(8)
            .and_then(|value| value.checked_add(1))
            .and_then(|value| value.checked_add(2))
            .and_then(|value| value.checked_add(entry.name.len()))
            .ok_or(EncodeError::ArithmeticOverflow)?;
        let new_length = bytes
            .len()
            .checked_add(record_length)
            .ok_or(EncodeError::ArithmeticOverflow)?;
        if new_length > maximum_data_bytes {
            break;
        }
        bytes.push(entry.qid.ty.bits());
        bytes.extend_from_slice(&entry.qid.version.to_le_bytes());
        bytes.extend_from_slice(&entry.qid.path.to_le_bytes());
        bytes.extend_from_slice(&entry.offset.to_le_bytes());
        bytes.push(entry.ty);
        bytes.extend_from_slice(&name_length.to_le_bytes());
        bytes.extend_from_slice(entry.name.as_bytes());
    }
    Ok(bytes)
}

fn write_timestamp(writer: &mut Writer, timestamp: Timestamp) -> Result<(), EncodeError> {
    writer.u64(timestamp.seconds)?;
    writer.u64(timestamp.nanoseconds)
}

fn write_lock(writer: &mut Writer, lock: &Lock) -> Result<(), EncodeError> {
    writer.u8(lock.ty as u8)?;
    writer.u64(lock.start)?;
    writer.u64(lock.length)?;
    writer.u32(lock.process_id)?;
    writer.string(&lock.client_id)
}

const fn map_structure_encode_error(error: StructureError) -> EncodeError {
    match error {
        StructureError::InvalidNanoseconds => EncodeError::InvalidValue("nanoseconds"),
        StructureError::UnknownMaskBits => EncodeError::InvalidValue("mask bits"),
        StructureError::InvalidComponentName => EncodeError::InvalidValue("path component"),
        StructureError::InvalidXattrName => EncodeError::InvalidValue("xattr name"),
        StructureError::ConflictingXattrFlags => EncodeError::InvalidValue("xattr flags"),
        StructureError::UnknownLockFlags => EncodeError::InvalidValue("lock flags"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_reader_is_little_endian_and_checked() {
        let mut reader = Reader::new(&[0x12, 0x56, 0x34, 8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(reader.u8(), Ok(0x12));
        assert_eq!(reader.u16(), Ok(0x3456));
        assert_eq!(reader.u64(), Ok(0x0102_0304_0506_0708));
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn primitive_writer_patches_checked_frame_size() {
        let mut writer = Writer::frame(MessageType::Rflush, Tag::new(0x1234), 9).unwrap();
        writer.u16(0xabcd).unwrap();
        assert_eq!(
            writer.finish().unwrap(),
            [9, 0, 0, 0, 109, 0x34, 0x12, 0xcd, 0xab]
        );
    }

    #[test]
    fn writer_refuses_to_cross_active_msize() {
        let mut writer = Writer::frame(MessageType::Rflush, Tag::new(1), 7).unwrap();
        assert!(matches!(
            writer.u8(1),
            Err(EncodeError::FrameTooLarge { .. })
        ));
    }
}
