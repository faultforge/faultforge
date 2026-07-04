//! The plugin integrity digest: `sha256(manifest_bytes ‖ executable_bytes)`.

use sha2::{Digest as _, Sha256};

/// Why a string is not a valid [`Digest`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DigestError {
    /// The string was not exactly 64 characters.
    #[error("digest must be 64 hex characters, got {0}")]
    WrongLength(usize),
    /// The string contained a character outside `[0-9a-f]`.
    #[error("digest must be lowercase hexadecimal")]
    NotLowercaseHex,
}

/// A plugin integrity digest: 64 lowercase hex characters (a sha256 sum).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Digest(String);

impl Digest {
    /// Parse and validate an externally-referenced digest string.
    ///
    /// # Errors
    ///
    /// Returns [`DigestError::WrongLength`] if not exactly 64 characters, or
    /// [`DigestError::NotLowercaseHex`] if any character is outside `[0-9a-f]`.
    pub fn parse(s: &str) -> Result<Self, DigestError> {
        if s.len() != 64 {
            return Err(DigestError::WrongLength(s.len()));
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(DigestError::NotLowercaseHex);
        }
        Ok(Self(s.to_string()))
    }

    /// Compute the digest over the manifest bytes immediately followed by the
    /// executable bytes, with no separator.
    ///
    /// Reproducible by an operator with `cat manifest.yaml <entrypoint> | sha256sum`.
    #[must_use]
    pub fn compute(manifest_bytes: &[u8], executable_bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(manifest_bytes);
        hasher.update(executable_bytes);
        Self(hex::encode(hasher.finalize()))
    }

    /// Compute the digest over `manifest_bytes` followed by every byte read from
    /// `executable`, streaming rather than buffering the whole executable.
    ///
    /// Identical result to [`Digest::compute`] with the same bytes, but constant
    /// memory — the entrypoint executable can be tens of megabytes and is hashed
    /// before every plugin invocation, so it is never loaded into a single
    /// allocation.
    ///
    /// # Errors
    ///
    /// Returns the IO error if reading from `executable` fails.
    pub fn compute_streaming(
        manifest_bytes: &[u8],
        executable: &mut impl std::io::Read,
    ) -> std::io::Result<Self> {
        let mut hasher = Sha256::new();
        hasher.update(manifest_bytes);
        std::io::copy(executable, &mut hasher)?;
        Ok(Self(hex::encode(hasher.finalize())))
    }

    /// The digest as a 64-character lowercase-hex string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cross-checked against coreutils:
    //   printf 'manifest-bytesexecutable-bytes' | shasum -a 256
    const VECTOR: &str = "aab0f7e0d67b3b0225252193a01c1313cf34da6a0244f59162eac97029d770a2";

    #[test]
    fn compute_matches_coreutils_vector() {
        let d = Digest::compute(b"manifest-bytes", b"executable-bytes");
        assert_eq!(d.as_str(), VECTOR);
    }

    #[test]
    fn compute_streaming_matches_compute() {
        let mut exe: &[u8] = b"executable-bytes";
        let streamed = Digest::compute_streaming(b"manifest-bytes", &mut exe).unwrap();
        assert_eq!(streamed.as_str(), VECTOR);
        assert_eq!(
            streamed,
            Digest::compute(b"manifest-bytes", b"executable-bytes")
        );
    }

    #[test]
    fn compute_is_plain_concatenation_across_the_boundary() {
        // sha256('abcdef'); the split point between the two inputs does not matter.
        const ABCDEF: &str = "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721";
        assert_eq!(Digest::compute(b"abc", b"def").as_str(), ABCDEF);
        assert_eq!(Digest::compute(b"ab", b"cdef").as_str(), ABCDEF);
    }

    #[test]
    fn any_byte_change_changes_the_digest() {
        let base = Digest::compute(b"manifest", b"exe");
        assert_ne!(base, Digest::compute(b"manifesX", b"exe"));
        assert_ne!(base, Digest::compute(b"manifest", b"exX"));
    }

    #[test]
    fn parse_accepts_a_valid_digest() {
        assert_eq!(Digest::parse(VECTOR).unwrap().as_str(), VECTOR);
    }

    #[test]
    fn parse_rejects_wrong_length() {
        assert_eq!(Digest::parse("abc"), Err(DigestError::WrongLength(3)));
    }

    #[test]
    fn parse_rejects_uppercase_and_non_hex() {
        let upper = VECTOR.to_uppercase();
        assert_eq!(Digest::parse(&upper), Err(DigestError::NotLowercaseHex));
        let non_hex = "g".repeat(64);
        assert_eq!(Digest::parse(&non_hex), Err(DigestError::NotLowercaseHex));
    }

    #[test]
    fn compute_then_parse_round_trips() {
        let d = Digest::compute(b"a", b"b");
        assert_eq!(Digest::parse(d.as_str()).unwrap(), d);
    }
}
