//! A `ByteRange` never runs backwards: whatever produced the two offsets, `start <= end` holds, so
//! `len`, `contains` and slicing with it cannot misbehave.

use satz_core::model::range::ByteRange;

#[test]
fn a_reversed_range_becomes_an_empty_one_at_its_start() {
    let r = ByteRange::new(5, 2);
    assert_eq!((r.start, r.end), (5, 5));
    assert_eq!(r.len(), 0);
    assert!(r.is_empty());
    assert!(!r.contains(5));
    assert!(!r.contains(2));
}

#[test]
fn ordinary_and_empty_ranges_are_unchanged() {
    let r = ByteRange::new(2, 5);
    assert_eq!((r.start, r.end, r.len()), (2, 5, 3));
    assert!(r.contains(2) && r.contains(4) && !r.contains(5));
    let empty = ByteRange::new(3, 3);
    assert!(empty.is_empty());
    assert_eq!((empty.start, empty.end), (3, 3));
}

#[test]
fn overlap_with_an_empty_or_clamped_range_is_consistent() {
    let a = ByteRange::new(0, 10);
    assert!(a.overlaps(&ByteRange::new(5, 6)));
    assert!(!a.overlaps(&ByteRange::new(10, 12)));
    let clamped = ByteRange::new(8, 3);
    let text = "0123456789abcdef";
    assert_eq!(&text[clamped.start..clamped.end], "");
    assert!(!clamped.overlaps(&ByteRange::new(0, 4)));
}

#[test]
fn extreme_offsets_do_not_panic() {
    let r = ByteRange::new(usize::MAX, 0);
    assert_eq!(r.len(), 0);
    let full = ByteRange::new(0, usize::MAX);
    assert_eq!(full.len(), usize::MAX);
    assert!(full.contains(usize::MAX - 1));
}
