#![allow(missing_docs)]

use w9pt_fs_storage::{
    BLOCK_SIZE, RangeError,
    layout::{BlockSpans, LogicalRange},
};

#[test]
fn empty_ranges_at_max_offset_do_not_underflow() {
    let range = LogicalRange::new(u64::MAX, 0).unwrap();
    assert!(range.is_empty());
    assert_eq!(BlockSpans::new(range).unwrap().next(), None);
    assert_eq!(range.clamp_to_eof(7).len(), 0);
}

#[test]
fn overflowing_end_is_rejected() {
    assert_eq!(
        LogicalRange::new(u64::MAX, 1),
        Err(RangeError::EndOverflow {
            offset: u64::MAX,
            length: 1,
        })
    );
}

#[test]
fn eof_clamping_preserves_only_visible_bytes() {
    let range = LogicalRange::new(7, 10).unwrap().clamp_to_eof(12);
    assert_eq!((range.start(), range.end(), range.len()), (7, 12, 5));
    assert_eq!(LogicalRange::new(12, 10).unwrap().clamp_to_eof(12).len(), 0);
}

#[test]
fn unaligned_cross_block_range_has_ordered_buffer_spans() {
    let block = u64::from(BLOCK_SIZE);
    let range = LogicalRange::new(block - 2, block + 5).unwrap();
    let spans = BlockSpans::new(range).unwrap().collect::<Vec<_>>();
    let values = spans
        .into_iter()
        .map(|span| {
            (
                span.block_index(),
                span.within_block(),
                span.buffer_offset(),
                span.len(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        vec![
            (0, BLOCK_SIZE - 2, 0, 2),
            (1, 0, 2, BLOCK_SIZE),
            (2, 0, BLOCK_SIZE as usize + 2, 3),
        ]
    );
}

#[test]
fn exact_full_block_is_identified_without_neighbor_spans() {
    let range = LogicalRange::new(u64::from(BLOCK_SIZE), u64::from(BLOCK_SIZE)).unwrap();
    let spans = BlockSpans::new(range).unwrap().collect::<Vec<_>>();
    assert_eq!(spans.len(), 1);
    assert!(spans[0].is_full_block());
    assert_eq!(spans[0].block_index(), 1);
}
