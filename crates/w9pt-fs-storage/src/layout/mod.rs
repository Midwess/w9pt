//! Shared checked logical-range planning and built-in content layouts.

mod block_split;
mod range;
mod raw;

pub(crate) use block_split::{
    create as create_block_split, read as read_block_split, truncate as truncate_block_split,
    truncate_from_new as truncate_block_split_from_new, write as write_block_split,
    write_from_new as write_block_split_from_new,
};
pub use range::{BlockSpan, BlockSpans, LogicalRange};
pub(crate) use raw::{
    create as create_raw, read as read_raw, truncate as truncate_raw,
    truncate_from_new as truncate_raw_from_new, write as write_raw,
    write_from_new as write_raw_from_new,
};
