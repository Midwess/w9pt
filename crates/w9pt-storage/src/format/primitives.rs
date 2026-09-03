//! Bounds-checked little-endian primitive codec.

use crate::FormatError;

#[derive(Debug)]
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    pub(crate) fn read_u8(&mut self) -> Result<u8, FormatError> {
        Ok(self.read_array::<1>()?[0])
    }

    pub(crate) fn read_u16(&mut self) -> Result<u16, FormatError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    pub(crate) fn read_u64(&mut self) -> Result<u64, FormatError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    pub(crate) fn read_array<const N: usize>(&mut self) -> Result<[u8; N], FormatError> {
        let end = self
            .position
            .checked_add(N)
            .ok_or(FormatError::ArithmeticOverflow { field: "reader" })?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(FormatError::Truncated)?;
        let mut output = [0; N];
        output.copy_from_slice(bytes);
        self.position = end;
        Ok(output)
    }

    pub(crate) fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], FormatError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(FormatError::ArithmeticOverflow { field: "reader" })?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(FormatError::Truncated)?;
        self.position = end;
        Ok(bytes)
    }

    pub(crate) fn finish(self) -> Result<(), FormatError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(FormatError::TrailingData)
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn write_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn write_u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn write_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn write_u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn write_bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_are_little_endian_and_checked() {
        let mut writer = Writer::new();
        writer.write_u8(0xaa);
        writer.write_u16(0x1122);
        writer.write_u32(0x3344_5566);
        writer.write_u64(0x7788_99aa_bbcc_ddee);
        let bytes = writer.into_bytes();

        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.read_u8(), Ok(0xaa));
        assert_eq!(reader.read_u16(), Ok(0x1122));
        assert_eq!(reader.read_u32(), Ok(0x3344_5566));
        assert_eq!(reader.read_u64(), Ok(0x7788_99aa_bbcc_ddee));
        assert_eq!(reader.remaining(), 0);
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn primitive_reader_rejects_truncation_and_trailing_bytes() {
        assert_eq!(Reader::new(&[1]).read_u16(), Err(FormatError::Truncated));
        assert_eq!(Reader::new(&[1]).finish(), Err(FormatError::TrailingData));
    }
}
