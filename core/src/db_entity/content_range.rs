/// Position of a value inside the bytes that hold it. Keys are addressed by
/// range rather than copied out, so reading `PartitionKey` / `RowKey` off a
/// stored row costs nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: usize,
    pub end: usize,
}

impl ContentRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }

    pub fn get_slice<'s>(&self, src: &'s [u8]) -> &'s [u8] {
        &src[self.start..self.end]
    }

    /// The bytes were verified to be valid UTF-8 when the entity was parsed, and
    /// nothing mutates them afterwards - the range keeps pointing at the same
    /// bytes for the whole life of the row.
    pub fn get_str<'s>(&self, src: &'s [u8]) -> &'s str {
        unsafe { std::str::from_utf8_unchecked(self.get_slice(src)) }
    }
}
