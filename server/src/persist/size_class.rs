/// Smallest slot size we ever allocate. 512 B == one disk sector, so an
/// in-place overwrite of a 512 B slot is atomic on virtually all drives.
pub const MIN_SIZE_CLASS: u32 = 512;

/// Picks the page-file (size class) for a slot that needs `needed` bytes total
/// (header + keys + payload). Classes are powers of two starting at
/// `MIN_SIZE_CLASS`, with no upper cap - a very large partition just lands in
/// its own large class. Worst-case internal waste is < 2x.
pub fn size_class_for(needed: usize) -> u32 {
    let mut size_class = MIN_SIZE_CLASS;

    while (size_class as usize) < needed {
        size_class = size_class
            .checked_mul(2)
            .expect("partition too large for any size class");
    }

    size_class
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries() {
        assert_eq!(size_class_for(0), 512);
        assert_eq!(size_class_for(1), 512);
        assert_eq!(size_class_for(512), 512);
        assert_eq!(size_class_for(513), 1024);
        assert_eq!(size_class_for(1024), 1024);
        assert_eq!(size_class_for(1025), 2048);
        assert_eq!(size_class_for(100_000), 131_072);
    }
}
