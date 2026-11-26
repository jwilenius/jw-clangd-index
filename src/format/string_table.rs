//! The `stri` chunk: u32le uncompressed size (0 = stored raw), then a zlib stream of NUL-terminated strings.
//!
//! Every other chunk refers to strings by their position in this table, so rewriting a string's
//! contents while keeping its position leaves those chunks valid.
//! clangd's reader does not require the table to be sorted.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

const UNCOMPRESSED_MARKER: u32 = 0;
/// clangd rejects tables whose uncompressed size exceeds this ratio to the compressed size.
const MAX_COMPRESSION_RATIO: usize = 1032;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableCompression {
    /// zlib level 1.
    Fast,
    /// zlib default level, as clangd writes it.
    Default,
    /// Raw strings, marked by uncompressed size 0.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StringTable {
    pub strings: Vec<Vec<u8>>,
}

impl StringTable {
    pub fn get(&self, index: u32) -> Result<&[u8]> {
        let Some(string) = self.strings.get(index as usize) else {
            bail!("string index {index} out of range ({} strings)", self.strings.len());
        };
        Ok(string)
    }
}

pub fn decode_string_table(chunk: &[u8]) -> Result<StringTable> {
    let Some((size_bytes, payload)) = chunk.split_first_chunk::<4>() else {
        bail!("string table truncated");
    };
    let uncompressed_size = u32::from_le_bytes(*size_bytes);
    let raw = match uncompressed_size {
        UNCOMPRESSED_MARKER => payload.to_vec(),
        size => inflate(payload, size as usize)?,
    };
    Ok(StringTable { strings: split_strings(&raw)? })
}

pub fn encode_string_table(table: &StringTable, compression: TableCompression) -> Result<Vec<u8>> {
    let raw = join_strings(table)?;
    let level = match compression {
        TableCompression::Fast => Compression::fast(),
        TableCompression::Default => Compression::default(),
        TableCompression::None => return Ok([&UNCOMPRESSED_MARKER.to_le_bytes()[..], &raw].concat()),
    };

    let mut out = Vec::with_capacity(raw.len() / 2);
    out.extend_from_slice(&u32::try_from(raw.len()).context("string table larger than 4 GiB")?.to_le_bytes());
    let mut encoder = ZlibEncoder::new(out, level);
    encoder.write_all(&raw)?;
    Ok(encoder.finish()?)
}

fn inflate(payload: &[u8], uncompressed_size: usize) -> Result<Vec<u8>> {
    let is_implausible = uncompressed_size / MAX_COMPRESSION_RATIO > payload.len();
    if is_implausible {
        bail!("implausible string table size {uncompressed_size}");
    }
    let mut raw = Vec::with_capacity(uncompressed_size);
    ZlibDecoder::new(payload).read_to_end(&mut raw).context("corrupt string table")?;
    if raw.len() != uncompressed_size {
        bail!("string table inflated to {} bytes, header says {uncompressed_size}", raw.len());
    }
    Ok(raw)
}

fn split_strings(raw: &[u8]) -> Result<Vec<Vec<u8>>> {
    let Some(body) = raw.strip_suffix(&[0]) else {
        bail!("string table not null terminated");
    };
    Ok(body.split(|&byte| byte == 0).map(<[u8]>::to_vec).collect())
}

fn join_strings(table: &StringTable) -> Result<Vec<u8>> {
    let total: usize = table.strings.iter().map(|string| string.len() + 1).sum();
    let mut raw = Vec::with_capacity(total);
    for string in &table.strings {
        if string.contains(&0) {
            bail!("string contains NUL and cannot be stored: {}", String::from_utf8_lossy(string));
        }
        raw.extend_from_slice(string);
        raw.push(0);
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_table() -> StringTable {
        let strings = ["", "file:///repo/a.h", "foo", "-I/repo/include"].map(|s| s.as_bytes().to_vec());
        StringTable { strings: strings.to_vec() }
    }

    #[test]
    fn round_trips_every_compression() {
        for compression in [TableCompression::Fast, TableCompression::Default, TableCompression::None] {
            let encoded = encode_string_table(&sample_table(), compression).unwrap();

            let decoded = decode_string_table(&encoded).unwrap();

            assert_eq!(decoded, sample_table(), "{compression:?}");
        }
    }

    #[test]
    fn marks_raw_table_with_zero_size() {
        let encoded = encode_string_table(&sample_table(), TableCompression::None).unwrap();

        assert_eq!(&encoded[..4], &[0, 0, 0, 0]);
        assert_eq!(&encoded[4..5], &[0]);
    }

    #[test]
    fn rejects_unterminated_raw_table() {
        assert!(decode_string_table(&[0, 0, 0, 0, b'a']).is_err());
    }

    #[test]
    fn rejects_string_with_nul() {
        let table = StringTable { strings: vec![b"a\0b".to_vec()] };

        assert!(encode_string_table(&table, TableCompression::Fast).is_err());
    }
}
