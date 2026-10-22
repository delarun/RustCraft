use crate::{ClassFormatError, Result};

/// Big-endian cursor over class file bytes.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos == self.data.len()
    }

    pub fn bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(len).filter(|&end| end <= self.data.len()).ok_or_else(|| {
            ClassFormatError::new(format!("truncated: need {len} bytes at offset {}", self.pos))
        })?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().unwrap())
    }

    pub fn u1(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u2(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub fn u4(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub fn u8(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    /// A u2-counted table of u2 values.
    pub fn u2_list(&mut self) -> Result<Vec<u16>> {
        let count = self.u2()?;
        (0..count).map(|_| self.u2()).collect()
    }
}
