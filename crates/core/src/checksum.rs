//! SHA-256 hashing, shared by the GUI (verifies a downloaded update package
//! before offering to install it) and the helper (re-verifies the same file
//! independently before running `pacman -U` — never trusts what the GUI
//! already checked).

use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 digest of `data`, e.g. for comparing against a
/// published `checksums.txt` line.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_empty_input_to_the_known_sha256_constant() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hashes_known_input_to_the_known_digest() {
        // Standard SHA-256 test vector for "abc".
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
