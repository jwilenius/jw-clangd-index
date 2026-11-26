//! Rewrites absolute paths under one worktree root to another, in raw paths and in `file:` URIs.
//!
//! A root matches only where a whole path starts and only up to a path boundary:
//! - the byte after the match must be `/` or the end of the string, so `/x/proj` never matches `/x/proj-wt`;
//!   in a raw string a token delimiter also ends the match (`-fdebug-prefix-map=/x/proj=.`, `-DROOT="/x/proj"`);
//! - in a URI the match must start the URI's path; elsewhere it may start mid-string (`-I/x/proj/include`,
//!   `--sysroot=/x/proj`), but not inside a longer path, so root `/src` never matches `/usr/src`.
//!
//! The raw start rule is a heuristic: a match starts a path when no `/` lies between it and the previous
//! token delimiter (or the string start). Raw strings carry no quoting we could trust, so it has known false matches:
//! - a space inside a path is taken as a delimiter, so root `/src` matches in `/home/my dir/src`;
//! - a relative path is not seen as one, so root `/x/proj` matches in `build/x/proj`.

use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use anyhow::{Result, bail};

use crate::format::uri::percent_encode;

const PATH_SEPARATOR: u8 = b'/';
const FILE_URI_PREFIX: &[u8] = b"file://";
/// Bytes that separate a path from surrounding text in a raw string, such as the `=` of `--sysroot=/x`.
const TOKEN_DELIMITERS: &[u8] = b" \t\n=,;:\"'";

/// One `old_root → new_root` mapping; both are absolute paths without a trailing slash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootMapping {
    pub old_root: Vec<u8>,
    pub new_root: Vec<u8>,
}

/// The mapping `old_root → new_root` with symlinks resolved in each root that exists, because clangd
/// stores real paths; a root that does not exist (yet) is kept as given.
pub fn canonical_root_mapping(old_root: &Path, new_root: &Path) -> RootMapping {
    RootMapping { old_root: canonical_path_bytes(old_root), new_root: canonical_path_bytes(new_root) }
}

fn canonical_path_bytes(path: &Path) -> Vec<u8> {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    resolved.into_os_string().into_vec()
}

#[derive(Clone, Debug)]
struct RootReplacement {
    from: Vec<u8>,
    to: Vec<u8>,
}

/// Root mappings compiled into raw-path and URI replacement patterns, each tried longest-first.
#[derive(Clone, Debug)]
pub struct PathMap {
    raw_replacements: Vec<RootReplacement>,
    uri_replacements: Vec<RootReplacement>,
}

impl PathMap {
    /// Compiles `mappings`; when two old roots are equal, the one listed first wins.
    pub fn new(mappings: &[RootMapping]) -> Result<Self> {
        for mapping in mappings {
            validate_root(&mapping.old_root)?;
            validate_root(&mapping.new_root)?;
        }
        let raw_replacements =
            mappings.iter().map(|mapping| RootReplacement { from: mapping.old_root.clone(), to: mapping.new_root.clone() });
        let uri_replacements = mappings
            .iter()
            .map(|mapping| RootReplacement { from: percent_encode(&mapping.old_root), to: percent_encode(&mapping.new_root) });
        Ok(Self { raw_replacements: longest_first(raw_replacements), uri_replacements: longest_first(uri_replacements) })
    }

    /// Returns the rewritten string, or `None` when no root matched (no allocation).
    ///
    /// A string starting with `file://` is a URI and uses the percent-encoded roots; any other string
    /// uses the raw roots. One left-to-right pass: replaced text is never rescanned, so a new root
    /// that contains the old one cannot apply twice.
    pub fn rewrite(&self, text: &[u8]) -> Option<Vec<u8>> {
        let is_uri = text.starts_with(FILE_URI_PREFIX);
        let replacements = if is_uri { &self.uri_replacements } else { &self.raw_replacements };
        let mut rewritten: Option<Vec<u8>> = None;
        let mut copied_until = 0;
        let mut position = 0;
        while let Some(offset) = text[position..].iter().position(|&byte| byte == PATH_SEPARATOR) {
            let match_start = position + offset;
            let is_candidate = is_path_start(text, match_start, is_uri);
            let replacement = if is_candidate { find_replacement(replacements, text, match_start, is_uri) } else { None };
            let Some(replacement) = replacement else {
                position = match_start + 1;
                continue;
            };
            let out = rewritten.get_or_insert_with(|| Vec::with_capacity(text.len() + replacement.to.len()));
            out.extend_from_slice(&text[copied_until..match_start]);
            out.extend_from_slice(&replacement.to);
            position = match_start + replacement.from.len();
            copied_until = position;
        }
        let mut out = rewritten?;
        out.extend_from_slice(&text[copied_until..]);
        Some(out)
    }
}

fn validate_root(root: &[u8]) -> Result<()> {
    let display = String::from_utf8_lossy(root);
    if root.first() != Some(&PATH_SEPARATOR) {
        bail!("root must be an absolute path: {display:?}");
    }
    if root.last() == Some(&PATH_SEPARATOR) {
        bail!("root must not end with '/': {display:?}");
    }
    if root.contains(&0) {
        bail!("root must not contain NUL: {display:?}");
    }
    Ok(())
}

/// Stable sort, so equal-length roots keep their listed order.
fn longest_first(replacements: impl Iterator<Item = RootReplacement>) -> Vec<RootReplacement> {
    let mut sorted: Vec<RootReplacement> = replacements.collect();
    sorted.sort_by_key(|replacement| std::cmp::Reverse(replacement.from.len()));
    sorted
}

fn find_replacement<'a>(replacements: &'a [RootReplacement], text: &[u8], start: usize, is_uri: bool) -> Option<&'a RootReplacement> {
    let rest = &text[start..];
    replacements.iter().find(|replacement| {
        let is_prefix = rest.starts_with(&replacement.from);
        is_prefix && is_path_end(rest, replacement.from.len(), is_uri)
    })
}

/// A path starts right after `file://` in a URI; in a raw string it starts when the text since the
/// previous delimiter holds no `/` (otherwise `start` lies inside a longer path).
fn is_path_start(text: &[u8], start: usize, is_uri: bool) -> bool {
    if is_uri {
        return start == FILE_URI_PREFIX.len();
    }
    let preceding = text[..start].iter().rev().find(|&&byte| byte == PATH_SEPARATOR || TOKEN_DELIMITERS.contains(&byte));
    preceding.is_none_or(|&byte| byte != PATH_SEPARATOR)
}

/// A path ends at `/` or the string end; in a raw string also at a token delimiter. URIs stay strict.
fn is_path_end(text: &[u8], position: usize, is_uri: bool) -> bool {
    text.get(position).is_none_or(|&byte| byte == PATH_SEPARATOR || (!is_uri && TOKEN_DELIMITERS.contains(&byte)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(old_root: &str, new_root: &str) -> RootMapping {
        RootMapping { old_root: old_root.as_bytes().to_vec(), new_root: new_root.as_bytes().to_vec() }
    }

    fn rewrite(mappings: &[RootMapping], text: &str) -> Option<String> {
        let path_map = PathMap::new(mappings).unwrap();
        path_map.rewrite(text.as_bytes()).map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn rewrites_root_and_exact_root() {
        let mappings = [mapping("/x/proj", "/y/wt")];

        assert_eq!(rewrite(&mappings, "/x/proj/src/a.cpp").as_deref(), Some("/y/wt/src/a.cpp"));
        assert_eq!(rewrite(&mappings, "/x/proj").as_deref(), Some("/y/wt"));
    }

    #[test]
    fn rejects_match_without_path_boundary() {
        let mappings = [mapping("/x/proj", "/y/wt")];

        assert_eq!(rewrite(&mappings, "/x/proj-wt/a.cpp"), None);
        assert_eq!(rewrite(&mappings, "/x/project"), None);
    }

    #[test]
    fn rewrites_match_in_middle_of_string() {
        let mappings = [mapping("/x/proj", "/y/wt")];

        let rewritten = rewrite(&mappings, "-I/x/proj/include");

        assert_eq!(rewritten.as_deref(), Some("-I/y/wt/include"));
    }

    #[test]
    fn rewrites_every_occurrence() {
        let mappings = [mapping("/x/proj", "/y/wt")];

        let rewritten = rewrite(&mappings, "/x/proj/a:/x/proj-wt/b:/x/proj");

        assert_eq!(rewritten.as_deref(), Some("/y/wt/a:/x/proj-wt/b:/y/wt"));
    }

    #[test]
    fn rejects_root_inside_longer_path() {
        let mappings = [mapping("/src", "/new")];

        assert_eq!(rewrite(&mappings, "/usr/src/linux/x.h"), None);
        assert_eq!(rewrite(&mappings, "-I/usr/src/include"), None);
        assert_eq!(rewrite(&mappings, "file:///usr/src/linux/x.h"), None);
    }

    #[test]
    fn rewrites_root_after_delimiter() {
        let mappings = [mapping("/x/proj", "/y/wt")];

        assert_eq!(rewrite(&mappings, "--sysroot=/x/proj").as_deref(), Some("--sysroot=/y/wt"));
        assert_eq!(rewrite(&mappings, "/usr/a /x/proj/b").as_deref(), Some("/usr/a /y/wt/b"));
    }

    #[test]
    fn ends_raw_match_at_token_delimiter() {
        let mappings = [mapping("/x/proj", "/y/wt")];

        assert_eq!(rewrite(&mappings, "-fdebug-prefix-map=/x/proj=.").as_deref(), Some("-fdebug-prefix-map=/y/wt=."));
        assert_eq!(rewrite(&mappings, "-DROOT=\"/x/proj\"").as_deref(), Some("-DROOT=\"/y/wt\""));
        assert_eq!(rewrite(&mappings, "-Wl,-rpath,/x/proj:/x/proj/lib").as_deref(), Some("-Wl,-rpath,/y/wt:/y/wt/lib"));
        assert_eq!(rewrite(&mappings, "-Wl,-rpath,/x/proj,--as-needed").as_deref(), Some("-Wl,-rpath,/y/wt,--as-needed"));
    }

    #[test]
    fn keeps_uri_match_strict_at_token_delimiter() {
        let mappings = [mapping("/x/proj", "/y/wt")];

        assert_eq!(rewrite(&mappings, "file:///x/proj:a.h"), None);
    }

    #[test]
    fn leaves_unrelated_string_unchanged() {
        let mappings = [mapping("/x/proj", "/y/wt")];

        assert_eq!(rewrite(&mappings, "/usr/include/stdio.h"), None);
        assert_eq!(rewrite(&mappings, ""), None);
    }

    #[test]
    fn rewrites_uri_with_percent_encoded_roots() {
        let mappings = [mapping("/x/my proj", "/y/c++ wt")];

        let rewritten = rewrite(&mappings, "file:///x/my%20proj/a.h");

        assert_eq!(rewritten.as_deref(), Some("file:///y/c%2B%2B%20wt/a.h"));
    }

    #[test]
    fn encodes_new_root_in_uri_even_when_old_root_needs_no_encoding() {
        let mappings = [mapping("/x/proj", "/y/my proj")];

        assert_eq!(rewrite(&mappings, "file:///x/proj/a.h").as_deref(), Some("file:///y/my%20proj/a.h"));
        assert_eq!(rewrite(&mappings, "-I/x/proj/include").as_deref(), Some("-I/y/my proj/include"));
    }

    #[test]
    fn rewrites_raw_path_with_space_in_root() {
        let mappings = [mapping("/x/my proj", "/y/wt")];

        assert_eq!(rewrite(&mappings, "/x/my proj/a.h").as_deref(), Some("/y/wt/a.h"));
        assert_eq!(rewrite(&mappings, "file:///x/my proj/a.h"), None);
    }

    #[test]
    fn prefers_longest_matching_root() {
        let mappings = [mapping("/x/proj", "/y/wt"), mapping("/x/proj/build", "/z/build")];

        assert_eq!(rewrite(&mappings, "/x/proj/build/gen.h").as_deref(), Some("/z/build/gen.h"));
        assert_eq!(rewrite(&mappings, "/x/proj/buildx/a.h").as_deref(), Some("/y/wt/buildx/a.h"));
    }

    #[test]
    fn applies_new_root_containing_old_root_once() {
        let mappings = [mapping("/x/proj", "/x/proj/x/proj")];

        let rewritten = rewrite(&mappings, "/x/proj/a.h");

        assert_eq!(rewritten.as_deref(), Some("/x/proj/x/proj/a.h"));
    }

    #[test]
    fn first_listed_wins_for_equal_old_roots() {
        let mappings = [mapping("/x/proj", "/first"), mapping("/x/proj", "/second")];

        assert_eq!(rewrite(&mappings, "/x/proj/a.h").as_deref(), Some("/first/a.h"));
    }

    #[test]
    fn rejects_invalid_roots() {
        for (old_root, new_root) in [("x/proj", "/y"), ("/x/proj/", "/y"), ("/x", "/y/"), ("/", "/y"), ("", "/y")] {
            let result = PathMap::new(&[mapping(old_root, new_root)]);

            assert!(result.is_err(), "{old_root:?} -> {new_root:?}");
        }
    }
}
