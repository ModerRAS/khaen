//! Tiny non-cryptographic RNG for protocol nonces, WebSocket masks and OAEP seeds.
//!
//! std-only: mixes the per-instance std RandomState keys with the wall clock and
//! a stack address (ASLR). This is NOT a CSPRNG; it is only used where RFC 6455
//! asks for unpredictable masking keys / nonces and RSA-OAEP needs a fresh seed.

use std::hash::{BuildHasher, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

/// Fill the buffer with pseudo-random bytes (see module docs).
pub(crate) fn fill_random(out: &mut [u8]) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let stack_probe = 0u8;
    let addr = (&stack_probe as *const u8) as usize as u128;
    for (chunk_index, chunk) in out.chunks_mut(8).enumerate() {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(now ^ (addr << 64) ^ ((chunk_index as u128) << 32));
        let word = hasher.finish().to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::fill_random;

    #[test]
    fn yields_varied_bytes() {
        let mut first = [0u8; 32];
        let mut second = [0u8; 32];
        fill_random(&mut first);
        fill_random(&mut second);
        assert_ne!(first, second);
        assert_ne!(first, [0u8; 32]);
    }

    #[test]
    fn fills_odd_lengths() {
        let mut buf = [0u8; 3];
        fill_random(&mut buf);
        let mut all_zero = true;
        for _ in 0..8 {
            fill_random(&mut buf);
            if buf.iter().any(|b| *b != 0) {
                all_zero = false;
                break;
            }
        }
        assert!(!all_zero);
    }
}
