use std::ops::Range;

/// Byte range within a document.
/// All parser outputs produce byte ranges, which are converted to LSP ranges via `LineIndex`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl ByteRange {
    #[inline]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    #[inline]
    pub fn contains(&self, offset: usize) -> bool {
        offset >= self.start && offset < self.end
    }

    #[inline]
    pub fn overlaps(&self, other: &ByteRange) -> bool {
        self.start < other.end && other.start < self.end
    }
}

impl From<Range<usize>> for ByteRange {
    #[inline]
    fn from(r: Range<usize>) -> Self {
        Self::new(r.start, r.end)
    }
}

impl From<ByteRange> for Range<usize> {
    #[inline]
    fn from(r: ByteRange) -> Self {
        r.start..r.end
    }
}
