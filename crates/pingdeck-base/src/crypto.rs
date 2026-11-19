//! Minimal SHA-256, HMAC-SHA256 and MD5 shared by request signing, content addressing and tokens,
//! plus SHA-1 and SHA-512 (and their HMACs) for the `CryptoJS` subset of request scripts.
//!
//! AWS Signature Version 4 needs SHA-256 and HMAC-SHA256; HTTP Digest authentication needs MD5 or
//! SHA-256; the attachment store and gateway client tokens hash with SHA-256. The digests are
//! correct for protocol use but not constant-time: compare secrets only through
//! [`constant_time_eq`], and never use these for password storage.

use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::error::AppError;

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// SHA-256 digest of `data`.
#[must_use]
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Lower-case hex SHA-256 digest of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    hex_lower(&sha256(data))
}

/// HMAC-SHA256 of `data` under `key`.
#[must_use]
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    // Normalize to a block-sized key to use RustCrypto's infallible constructor.
    let mut key_block = [0_u8; 64];
    if key.len() > key_block.len() {
        key_block[..32].copy_from_slice(&sha256(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut mac = <Hmac<Sha256> as Mac>::new((&key_block).into());
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// SHA-1 digest of `data` (script compatibility only; not collision resistant).
#[must_use]
pub fn sha1(data: &[u8]) -> [u8; 20] {
    Sha1::digest(data).into()
}

/// SHA-512 digest of `data`.
#[must_use]
pub fn sha512(data: &[u8]) -> [u8; 64] {
    Sha512::digest(data).into()
}

/// HMAC-SHA1 of `data` under `key`.
#[must_use]
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    // RFC 2104: a key longer than the block is hashed, then every key is zero-padded to it.
    let mut key_block = [0_u8; 64];
    if key.len() > key_block.len() {
        key_block[..20].copy_from_slice(&sha1(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut mac = <Hmac<Sha1> as Mac>::new((&key_block).into());
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// HMAC-SHA512 of `data` under `key`.
#[must_use]
pub fn hmac_sha512(key: &[u8], data: &[u8]) -> [u8; 64] {
    let mut key_block = [0_u8; 128];
    if key.len() > key_block.len() {
        key_block[..64].copy_from_slice(&sha512(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut mac = <Hmac<Sha512> as Mac>::new((&key_block).into());
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// MD5 digest of `data` (HTTP Digest authentication only; not collision resistant).
#[must_use]
pub fn md5(data: &[u8]) -> [u8; 16] {
    Md5::digest(data).into()
}

/// Compares two byte strings in time that depends only on their length, for tokens and digests.
#[inline]
#[must_use]
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

/// Lower-case hexadecimal encoding of `bytes`.
#[inline]
#[must_use]
pub fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

/// Fills `bytes` from the operating system CSPRNG; fails explicitly when entropy is unavailable.
///
/// # Errors
/// [`AppError::Internal`] when the OS CSPRNG call fails.
pub fn fill_random(bytes: &mut [u8]) -> Result<(), AppError> {
    getrandom::fill(bytes)
        .map_err(|error| AppError::internal(format!("no OS randomness for a token: {error}")))
}

/// `N` random bytes from the OS CSPRNG encoded as lower-case hex.
///
/// # Errors
/// [`AppError::Internal`] when the OS CSPRNG call fails.
pub fn random_hex<const N: usize>() -> Result<String, AppError> {
    let mut bytes = [0_u8; N];
    fill_random(&mut bytes)?;
    Ok(hex_lower(&bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        constant_time_eq, hex_lower, hmac_sha1, hmac_sha256, hmac_sha512, md5, random_hex, sha1,
        sha256_hex, sha512,
    };

    // RFC 4231 test case 6 (a 131-byte key, longer than both blocks) and FIPS 180 "abc".
    #[test]
    fn sha1_sha512_and_their_hmacs_match_standard_vectors() {
        assert_eq!(
            hex_lower(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert!(hex_lower(&sha512(b"abc")).starts_with("ddaf35a193617aba"));
        let fox = b"The quick brown fox jumps over the lazy dog";
        assert_eq!(
            hex_lower(&hmac_sha1(b"key", fox)),
            "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9"
        );
        assert!(hex_lower(&hmac_sha512(b"key", fox)).starts_with("b42af09057bac1e2"));
        let long_key = [0xaa_u8; 131];
        let message = b"Test Using Larger Than Block-Size Key - Hash Key First";
        assert!(hex_lower(&hmac_sha512(&long_key, message)).starts_with("80b24263c7c1a3eb"));
        assert_eq!(
            hex_lower(&hmac_sha1(&[0xaa_u8; 80], message)),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }

    #[test]
    fn digests_and_helpers_match_standard_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_lower(&hmac_sha256(
                b"key",
                b"The quick brown fox jumps over the lazy dog"
            )),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
        assert_eq!(
            hex_lower(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert_eq!(hex_lower(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"other"));
        assert!(!constant_time_eq(b"token", b"tokens"));
    }

    // Empty input and the 55/56/64-byte lengths straddle SHA-256's single-block padding limit.
    #[test]
    fn digests_match_padding_boundary_vectors() {
        let cases: [(usize, &str); 4] = [
            (
                0,
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                55,
                "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
            ),
            (
                56,
                "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
            ),
            (
                64,
                "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
            ),
        ];
        for (len, expected) in cases {
            assert_eq!(sha256_hex(&vec![b'a'; len]), expected, "{len} bytes");
        }
        assert_eq!(hex_lower(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    }

    // Ledger (`ap_`) and gateway client (`cl_`) ids rely on this shape: 2N lower-case hex chars.
    #[test]
    fn random_hex_is_two_lower_hex_chars_per_byte_and_varies() {
        let first = random_hex::<6>().unwrap();
        let second = random_hex::<6>().unwrap();
        for value in [&first, &second] {
            assert_eq!(value.len(), 12, "{value}");
            assert!(
                value
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
                "{value}"
            );
        }
        assert_ne!(first, second);
    }
}
