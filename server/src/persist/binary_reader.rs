/// Cursor over the fixed-width binary layouts this module writes. Every read is
/// bounds-checked, so a truncated file is reported instead of panicking.
pub struct BinaryReader<'s> {
    src: &'s [u8],
    pos: usize,
}

impl<'s> BinaryReader<'s> {
    pub fn new(src: &'s [u8]) -> Self {
        Self { src, pos: 0 }
    }

    pub fn is_eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn take(&mut self, len: usize) -> Result<&'s [u8], String> {
        let Some(end) = self.pos.checked_add(len) else {
            return Err("length overflow".to_string());
        };

        if end > self.src.len() {
            return Err(format!(
                "need {} bytes at offset {} but only {} are there",
                len,
                self.pos,
                self.src.len() - self.pos
            ));
        }

        let result = &self.src[self.pos..end];
        self.pos = end;
        Ok(result)
    }

    pub fn read_u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn read_u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn read_u32_prefixed_bytes(&mut self) -> Result<Vec<u8>, String> {
        let len = self.read_u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }
}

pub fn write_u32_prefixed_bytes(dest: &mut Vec<u8>, src: &[u8]) {
    dest.extend_from_slice(&(src.len() as u32).to_le_bytes());
    dest.extend_from_slice(src);
}
