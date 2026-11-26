//! Byte cursor and LEB128-style `u32` varints as clangd's `Serialization.cpp` reads and writes them.

use anyhow::{Result, bail};

const CONTINUATION_BIT: u8 = 0x80;
const PAYLOAD_MASK: u8 = 0x7f;
const MAX_SHIFT: u32 = 32;

/// Little-endian byte cursor with clangd's bounds rules.
pub struct ByteReader<'a> {
    bytes: &'a [u8],
}

impl<'a> ByteReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len()
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        let Some((&first, rest)) = self.bytes.split_first() else {
            bail!("unexpected end of data");
        };
        self.bytes = rest;
        Ok(first)
    }

    pub fn read_bytes(&mut self, count: usize) -> Result<&'a [u8]> {
        if count > self.bytes.len() {
            bail!("unexpected end of data: need {count} bytes, have {}", self.bytes.len());
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(taken)
    }

    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let bytes = self.read_bytes(N)?;
        Ok(bytes.try_into().expect("read_bytes returned N bytes"))
    }

    /// Reads a varint; like clangd, at most 5 bytes and bits above 32 are dropped.
    pub fn read_var(&mut self) -> Result<u32> {
        let mut value: u32 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = self.read_u8()?;
            value |= u32::from(byte & PAYLOAD_MASK).wrapping_shl(shift);
            shift += 7;
            let is_last = byte & CONTINUATION_BIT == 0 || shift >= MAX_SHIFT;
            if is_last {
                return Ok(value);
            }
        }
    }

    /// Reads a varint count and rejects counts larger than the remaining bytes (clangd's `consumeSize`).
    pub fn read_count(&mut self) -> Result<usize> {
        let count = self.read_var()? as usize;
        if count > self.bytes.len() {
            bail!("implausible count {count} with {} bytes left", self.bytes.len());
        }
        Ok(count)
    }
}

pub fn write_var(out: &mut Vec<u8>, mut value: u32) {
    while value > u32::from(PAYLOAD_MASK) {
        out.push((value as u8 & PAYLOAD_MASK) | CONTINUATION_BIT);
        value >>= 7;
    }
    out.push(value as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_clangd_documented_example() {
        let mut reader = ByteReader::new(&[0x9a, 0x2f]);

        let value = reader.read_var().unwrap();

        assert_eq!(value, 6042);
        assert!(reader.is_empty());
    }

    #[test]
    fn round_trips_boundary_values() {
        for value in [0, 1, 127, 128, 16383, 16384, u32::MAX] {
            let mut encoded = Vec::new();

            write_var(&mut encoded, value);

            assert_eq!(ByteReader::new(&encoded).read_var().unwrap(), value, "value {value}");
        }
    }

    #[test]
    fn rejects_count_larger_than_remaining_bytes() {
        let mut reader = ByteReader::new(&[5, 0, 0]);

        assert!(reader.read_count().is_err());
    }

    #[test]
    fn rejects_truncated_varint() {
        assert!(ByteReader::new(&[0x80]).read_var().is_err());
    }
}
