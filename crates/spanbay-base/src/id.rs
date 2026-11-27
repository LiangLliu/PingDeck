//! Random identifiers: entity UUIDs and the `<prefix>_<hex>` handles of runs, requests and
//! processes. Ids are handles, never secrets; tokens draw from `getrandom` directly and fail
//! without entropy instead of falling back.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto::{hex_lower, sha256};

/// `N` (at most 32) bytes from the OS CSPRNG. If the OS refuses, the bytes come from hashing the
/// clock and a process-wide counter, so id creation stays infallible and ids stay distinct.
#[must_use]
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    if getrandom::fill(&mut bytes).is_err() {
        static FALLBACK: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let count = FALLBACK.fetch_add(1, Ordering::Relaxed);
        let digest = sha256(format!("{nanos}:{count}").as_bytes());
        let len = N.min(digest.len());
        bytes[..len].copy_from_slice(&digest[..len]);
    }
    bytes
}

/// A random (version 4) UUID in lower-case hyphenated form.
#[must_use]
pub fn uuid_v4() -> String {
    uuid::Builder::from_random_bytes(random_bytes())
        .into_uuid()
        .hyphenated()
        .to_string()
}

/// `<prefix>_<2N lower-case hex digits>` from `N` random bytes.
#[must_use]
pub fn prefixed_hex<const N: usize>(prefix: &str) -> String {
    format!("{prefix}_{}", hex_lower(&random_bytes::<N>()))
}

#[cfg(test)]
mod tests {
    use super::{prefixed_hex, uuid_v4};

    #[test]
    fn ids_have_expected_wire_format_and_vary() {
        let a = uuid_v4();
        let b = uuid_v4();
        assert_ne!(a, b);
        let bytes = a.as_bytes();
        assert_eq!(bytes.len(), 36, "{a}");
        assert_eq!(bytes[14], b'4', "{a}");
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'), "{a}");

        let id = prefixed_hex::<6>("proc");
        assert!(id.len() == 17 && id.starts_with("proc_"), "{id}");
        assert!(
            id[5..]
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
        );
    }
}
