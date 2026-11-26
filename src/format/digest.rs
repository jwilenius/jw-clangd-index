//! clangd's `digest()` (`clangd/SourceCode.cpp`): XXH3-64 packed least-significant byte first.
//!
//! Hashes file contents (staleness check) and absolute file paths (shard file names).

use xxhash_rust::xxh3::xxh3_64;

pub const DIGEST_SIZE: usize = 8;

pub type FileDigest = [u8; DIGEST_SIZE];

pub fn digest(bytes: &[u8]) -> FileDigest {
    xxh3_64(bytes).to_le_bytes()
}

/// Upper-case hex of the digest bytes in array order, as `llvm::toHex` prints it.
pub fn digest_hex(digest: &FileDigest) -> String {
    digest.iter().map(|byte| format!("{byte:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// XXH3-64 of the empty input is `0x2D06800538D394C2` (xxHash reference test vector).
    #[test]
    fn prints_reference_hash_lsb_first_in_upper_case() {
        let hex = digest_hex(&digest(b""));

        assert_eq!(hex, "C294D3380580062D");
    }
}
