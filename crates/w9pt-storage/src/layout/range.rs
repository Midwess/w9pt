//! Overflow-safe logical byte ranges and block spans.

use crate::{BLOCK_SIZE_V1, RangeError};

/// Checked half-open logical byte range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogicalRange {
    start: u64,
    end: u64,
}

impl LogicalRange {
    /// Creates `[offset, offset + length)`, rejecting arithmetic overflow.
    pub const fn new(offset: u64, length: u64) -> Result<Self, RangeError> {
        match offset.checked_add(length) {
            Some(end) => Ok(Self { start: offset, end }),
            None => Err(RangeError::EndOverflow { offset, length }),
        }
    }

    /// Creates a logical range from an in-memory buffer length.
    pub fn from_usize(offset: u64, length: usize) -> Result<Self, RangeError> {
        let length = u64::try_from(length).map_err(|_| RangeError::LengthConversion)?;
        Self::new(offset, length)
    }

    /// Returns the inclusive logical start.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the exclusive logical end.
    pub const fn end(self) -> u64 {
        self.end
    }

    /// Returns the checked byte count.
    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    /// Reports whether the range contains no bytes.
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Clamps the range to logical EOF without changing its requested start.
    pub const fn clamp_to_eof(self, logical_size: u64) -> Self {
        if self.start >= logical_size {
            Self {
                start: self.start,
                end: self.start,
            }
        } else if self.end > logical_size {
            Self {
                start: self.start,
                end: logical_size,
            }
        } else {
            self
        }
    }
}

/// One requested span within a version-1 logical block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockSpan {
    block_index: u64,
    within_block: u32,
    buffer_offset: usize,
    len: u32,
}

impl BlockSpan {
    /// Returns the file-relative block index.
    pub const fn block_index(self) -> u64 {
        self.block_index
    }

    /// Returns the byte offset within the canonical block.
    pub const fn within_block(self) -> u32 {
        self.within_block
    }

    /// Returns the byte offset in the caller's read/write buffer.
    pub const fn buffer_offset(self) -> usize {
        self.buffer_offset
    }

    /// Returns the byte count covered by this span.
    pub const fn len(self) -> u32 {
        self.len
    }

    /// Reports whether this span contains no bytes.
    ///
    /// Planned spans are always nonempty; this method accompanies [`Self::len`]
    /// for conventional collection-like inspection.
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Reports whether this span covers an entire version-1 block.
    pub const fn is_full_block(self) -> bool {
        self.within_block == 0 && self.len == BLOCK_SIZE_V1
    }
}

/// Lazy iterator over version-1 block spans for one checked logical range.
#[derive(Clone, Debug)]
pub struct BlockSpans {
    absolute: u64,
    remaining: u64,
    buffer_offset: usize,
}

impl BlockSpans {
    /// Creates a lazy planner, rejecting ranges whose buffer offset cannot fit `usize`.
    pub fn new(range: LogicalRange) -> Result<Self, RangeError> {
        usize::try_from(range.len()).map_err(|_| RangeError::LengthConversion)?;
        Ok(Self {
            absolute: range.start,
            remaining: range.len(),
            buffer_offset: 0,
        })
    }
}

impl Iterator for BlockSpans {
    type Item = BlockSpan;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let block_size = u64::from(BLOCK_SIZE_V1);
        let block_index = self.absolute / block_size;
        let within = self.absolute % block_size;
        let len = self.remaining.min(block_size - within);
        let span = BlockSpan {
            block_index,
            within_block: u32::try_from(within).ok()?,
            buffer_offset: self.buffer_offset,
            len: u32::try_from(len).ok()?,
        };
        self.absolute = self.absolute.checked_add(len)?;
        self.remaining -= len;
        self.buffer_offset = self.buffer_offset.checked_add(usize::try_from(len).ok()?)?;
        Some(span)
    }
}
