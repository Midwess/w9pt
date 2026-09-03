//! Shared checked logical-range planning and built-in content layouts.

mod block_split;
mod range;
mod raw;

pub(crate) use block_split::{
    create as create_block_split, read as read_block_split, truncate as truncate_block_split,
    write as write_block_split,
};
pub use range::{BlockSpan, BlockSpans, LogicalRange};
pub(crate) use raw::{
    create as create_raw, read as read_raw, truncate as truncate_raw, write as write_raw,
};
