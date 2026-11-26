//! `file:` URIs exactly as clangd writes and reads them (`clangd/URI.cpp`).

const FILE_URI_PREFIX: &[u8] = b"file://";

/// `file://` + percent-encoded absolute path, as `URI::create(path).toString()`.
pub fn path_to_file_uri(path: &[u8]) -> Vec<u8> {
    let mut uri = FILE_URI_PREFIX.to_vec();
    uri.extend_from_slice(&percent_encode(path));
    uri
}

/// Absolute path of a `file://` URI, or `None` for other schemes.
pub fn file_uri_to_path(uri: &[u8]) -> Option<Vec<u8>> {
    let body = uri.strip_prefix(FILE_URI_PREFIX)?;
    Some(percent_decode(body))
}

pub fn percent_encode(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len());
    for &byte in content {
        if !should_escape(byte) {
            out.push(byte);
            continue;
        }
        out.extend_from_slice(format!("%{byte:02X}").as_bytes());
    }
    out
}

fn should_escape(byte: u8) -> bool {
    let is_unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/' | b':');
    !is_unreserved
}

/// Mirrors clangd's `percentDecode`, including that an escape without two following bytes stays literal.
fn percent_decode(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len());
    let mut index = 0;
    while index < content.len() {
        let byte = content[index];
        let is_escape =
            byte == b'%' && index + 2 < content.len() && content[index + 1].is_ascii_hexdigit() && content[index + 2].is_ascii_hexdigit();
        if !is_escape {
            out.push(byte);
            index += 1;
            continue;
        }
        out.push(hex_value(content[index + 1]) << 4 | hex_value(content[index + 2]));
        index += 3;
    }
    out
}

fn hex_value(digit: u8) -> u8 {
    (digit as char).to_digit(16).expect("caller checked hex digit") as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_reserved_characters_in_upper_case() {
        let uri = path_to_file_uri(b"/home/me/my proj/a+b.h");

        assert_eq!(uri, b"file:///home/me/my%20proj/a%2Bb.h");
    }

    #[test]
    fn keeps_unreserved_characters() {
        assert_eq!(percent_encode(b"/a-b_c.d~e:f/G9"), b"/a-b_c.d~e:f/G9");
    }

    #[test]
    fn decodes_what_it_encodes() {
        let path = b"/x/with space/f%oo.h";

        let decoded = file_uri_to_path(&path_to_file_uri(path)).unwrap();

        assert_eq!(decoded, path);
    }

    #[test]
    fn decodes_escape_at_string_end() {
        assert_eq!(percent_decode(b"/a%2B"), b"/a+");
    }

    #[test]
    fn keeps_truncated_escape_literal() {
        assert_eq!(percent_decode(b"/a%2"), b"/a%2");
    }

    #[test]
    fn rejects_other_schemes() {
        assert_eq!(file_uri_to_path(b"test:///a.h"), None);
    }
}
