//! 自实现 SHA-256（FIPS 180-4），零外部依赖。用于夹具 WAV 完整性校验。

const INITIAL: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const ROUND_CONSTANTS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 digest of an in-memory byte slice.
pub fn digest(data: &[u8]) -> [u8; 32] {
    let bit_length = (data.len() as u64).wrapping_mul(8);

    // Padding: message + 0x80 + zeros + 8-byte big-endian bit length, to a multiple of 64 bytes.
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    let mut block = [0u8; 64];
    for chunk in padded.chunks_exact(64) {
        block.copy_from_slice(chunk);
        compress(&mut state, &block);
    }

    let mut out = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    let mut f = state[5];
    let mut g = state[6];
    let mut h = state[7];

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(ROUND_CONSTANTS[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// Lower-case hex of a digest.
pub fn to_hex(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Lower-case hex SHA-256 of an in-memory byte slice.
pub fn hex_digest(data: &[u8]) -> String {
    to_hex(&digest(data))
}

/// SHA-256 of a file's raw bytes (streamed in 64 KiB chunks).
pub fn file_digest(path: &std::path::Path) -> Result<[u8; 32], String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("cannot open {} for SHA-256: {error}", path.display()))?;
    let mut buffer = [0u8; 64 * 1024];
    let mut collected = Vec::new();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {} for SHA-256: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        collected.extend_from_slice(&buffer[..read]);
    }
    Ok(digest(&collected))
}

pub fn file_hex(path: &std::path::Path) -> Result<String, String> {
    Ok(to_hex(&file_digest(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R:\Temp 在本机不可写；先探测 current_dir（cargo test 的包根），失败再退回 temp_dir。
    fn scratch_path(name: &str) -> std::path::PathBuf {
        let file_name = format!(
            "tspi-room-sim-sha256-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let base = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let candidate = base.join(&file_name);
        match std::fs::write(&candidate, b"probe") {
            Ok(()) => {
                let _ = std::fs::remove_file(&candidate);
                candidate
            }
            Err(_) => std::env::temp_dir().join(file_name),
        }
    }

    #[test]
    fn standard_vectors() {
        assert_eq!(
            hex_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex_digest(b"The quick brown fox jumps over the lazy dog"),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
    }

    #[test]
    fn million_a_vector() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(
            hex_digest(&data),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn length_boundaries() {
        // 55/56/64 boundary exercise both the padding branch and a full extra block.
        for length in [55usize, 56, 63, 64, 65, 127, 128] {
            let data = vec![0x5au8; length];
            let hex = hex_digest(&data);
            assert_eq!(hex.len(), 64);
            // Deterministic: same bytes -> same hex.
            assert_eq!(hex, hex_digest(&data));
        }
    }

    #[test]
    fn file_digest_matches_in_memory_digest() {
        let path = scratch_path("sha256.bin");
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        assert_eq!(file_hex(&path).unwrap(), hex_digest(&data));
        let _ = std::fs::remove_file(&path);
    }
}
