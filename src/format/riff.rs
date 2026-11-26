//! RIFF container used by clangd index files (`clangd/RIFF.cpp`).
//!
//! Layout: `RIFF` u32le(length) fourcc(type), then chunks `fourcc u32le(length) data [pad byte if odd]`.

use std::borrow::Cow;

use anyhow::{Result, bail};

use super::bytes::ByteReader;

pub type FourCc = [u8; 4];

const RIFF_MAGIC: &FourCc = b"RIFF";
const FOURCC_SIZE: usize = 4;
const CHUNK_HEADER_SIZE: usize = 8;

pub struct Chunk<'a> {
    pub id: FourCc,
    pub data: Cow<'a, [u8]>,
}

pub struct RiffFile<'a> {
    pub file_type: FourCc,
    pub chunks: Vec<Chunk<'a>>,
}

impl<'a> RiffFile<'a> {
    pub fn chunk(&self, id: &FourCc) -> Option<&[u8]> {
        self.chunks.iter().find(|chunk| &chunk.id == id).map(|chunk| chunk.data.as_ref())
    }

    pub fn replace_chunk(&mut self, id: &FourCc, data: Vec<u8>) {
        if let Some(chunk) = self.chunks.iter_mut().find(|chunk| &chunk.id == id) {
            chunk.data = Cow::Owned(data);
        }
    }
}

/// Parses a RIFF file; chunk data borrows from `bytes`. Trailing bytes after the outer chunk are ignored.
pub fn parse_riff(bytes: &[u8]) -> Result<RiffFile<'_>> {
    let mut reader = ByteReader::new(bytes);
    let outer = read_chunk(&mut reader)?;
    if &outer.id != RIFF_MAGIC {
        bail!("not a RIFF file");
    }

    let mut body = ByteReader::new(outer.data);
    let file_type: FourCc = body.read_array()?;
    let mut chunks = Vec::new();
    while !body.is_empty() {
        let chunk = read_chunk(&mut body)?;
        chunks.push(Chunk { id: chunk.id, data: Cow::Borrowed(chunk.data) });
    }
    Ok(RiffFile { file_type, chunks })
}

pub fn write_riff(file: &RiffFile) -> Vec<u8> {
    let body_size: usize = FOURCC_SIZE + file.chunks.iter().map(|chunk| padded_chunk_size(&chunk.data)).sum::<usize>();
    let mut out = Vec::with_capacity(CHUNK_HEADER_SIZE + body_size);
    out.extend_from_slice(RIFF_MAGIC);
    out.extend_from_slice(&chunk_length(body_size).to_le_bytes());
    out.extend_from_slice(&file.file_type);
    for chunk in &file.chunks {
        out.extend_from_slice(&chunk.id);
        out.extend_from_slice(&chunk_length(chunk.data.len()).to_le_bytes());
        out.extend_from_slice(&chunk.data);
        if chunk.data.len() % 2 == 1 {
            out.push(0);
        }
    }
    out
}

struct RawChunk<'a> {
    id: FourCc,
    data: &'a [u8],
}

fn read_chunk<'a>(reader: &mut ByteReader<'a>) -> Result<RawChunk<'a>> {
    let id: FourCc = reader.read_array()?;
    let length = u32::from_le_bytes(reader.read_array()?) as usize;
    let data = reader.read_bytes(length)?;
    let has_padding = length % 2 == 1 && !reader.is_empty();
    if has_padding && reader.read_u8()? != 0 {
        bail!("nonzero padding byte after chunk {}", String::from_utf8_lossy(&id));
    }
    Ok(RawChunk { id, data })
}

fn padded_chunk_size(data: &[u8]) -> usize {
    CHUNK_HEADER_SIZE + data.len() + data.len() % 2
}

fn chunk_length(size: usize) -> u32 {
    u32::try_from(size).expect("RIFF chunk larger than 4 GiB")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_file() -> RiffFile<'static> {
        RiffFile {
            file_type: *b"CdIx",
            chunks: vec![
                Chunk { id: *b"meta", data: Cow::Owned(vec![21, 0, 0, 0]) },
                Chunk { id: *b"odd!", data: Cow::Owned(vec![1, 2, 3]) },
            ],
        }
    }

    #[test]
    fn round_trips_chunks_with_odd_padding() {
        let bytes = write_riff(&sample_file());

        let parsed = parse_riff(&bytes).unwrap();

        assert_eq!(&parsed.file_type, b"CdIx");
        assert_eq!(parsed.chunk(b"meta").unwrap(), &[21, 0, 0, 0]);
        assert_eq!(parsed.chunk(b"odd!").unwrap(), &[1, 2, 3]);
        assert_eq!(bytes.len(), 12 + 12 + 12);
    }

    #[test]
    fn writes_outer_length_covering_type_and_padded_chunks() {
        let bytes = write_riff(&sample_file());

        let outer_length = u32::from_le_bytes(bytes[4..8].try_into().unwrap());

        assert_eq!(outer_length as usize, bytes.len() - 8);
    }

    #[test]
    fn rejects_nonzero_padding() {
        let mut bytes = write_riff(&sample_file());
        *bytes.last_mut().unwrap() = 7;

        assert!(parse_riff(&bytes).is_err());
    }

    #[test]
    fn rejects_non_riff_data() {
        assert!(parse_riff(b"JUNK\x04\x00\x00\x00abcd").is_err());
    }
}
