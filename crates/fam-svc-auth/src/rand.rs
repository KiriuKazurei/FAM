//! The machine's random bytes, for token secrets and salts.
//!
//! Nothing in this workspace had a use for one until now, and a token secret is the one value here
//! that cannot be derived from anything else: a guessable secret is not a slow path, it is a full
//! compromise of whatever the token can reach. So this calls the operating system rather than mixing
//! a clock with a process id, and it has no fallback - if the system generator cannot be reached,
//! the caller is told and no token is issued.

/// Bytes to fill, from the operating system's generator.
///
/// Windows: `BCryptGenRandom` with the system-preferred flag, which needs no provider handle and no
/// algorithm name to get wrong. Everywhere else: `/dev/urandom`, which is blocking-by-contract on
/// every system this program is built for.
pub fn fill(buffer: &mut [u8]) -> Result<(), crate::Error> {
    if buffer.is_empty() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        let length = u32::try_from(buffer.len()).map_err(|_| crate::Error::EntropyUnavailable)?;
        // Safety: the buffer is a valid, writable slice for `length` bytes, and the system-preferred
        // flag means no handle is involved. The return value is the only thing that can go wrong.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buffer.as_mut_ptr(),
                length,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(crate::Error::EntropyUnavailable)
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Read;
        let mut file = std::fs::File::open("/dev/urandom").map_err(|source| {
            crate::Error::EntropyUnavailableWith {
                reason: source.to_string(),
            }
        })?;
        file.read_exact(buffer)
            .map_err(|source| crate::Error::EntropyUnavailableWith {
                reason: source.to_string(),
            })
    }
}

/// A fresh value as lowercase hex, which is the form that survives being pasted, quoted and stored
/// in TOML.
pub fn hex_bytes(count: usize) -> Result<String, crate::Error> {
    let mut buffer = vec![0u8; count];
    fill(&mut buffer)?;
    Ok(fam_core::hex_lower(&buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_satisfied_in_the_requested_length() {
        for count in [1usize, 4, 16, 32, 64] {
            let text = hex_bytes(count).expect("the machine has entropy");
            assert_eq!(text.len(), count * 2, "{count} bytes as hex");
            assert!(
                text.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "lowercase hex only: {text}"
            );
        }
    }

    /// Not a statistical test - that would be a test of the operating system - just the assertion
    /// that matters to a token store: nothing repeats, and nothing looks derived from a counter.
    #[test]
    fn two_calls_are_not_the_same_value() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(
                seen.insert(hex_bytes(16).expect("entropy")),
                "a repeated 128-bit value from the system generator"
            );
        }
        let mut adjacent = [
            hex_bytes(8).expect("entropy"),
            hex_bytes(8).expect("entropy"),
        ];
        adjacent.sort();
        assert_ne!(adjacent[0], adjacent[1]);
    }

    #[test]
    fn filling_nothing_is_not_an_error() {
        assert!(fill(&mut []).is_ok());
    }
}
