//! Minimal dependency-free crypto needed by the CueMix control path:
//! base64, SHA-1 (WebSocket accept + OAEP/MGF1), SHA-256 (password digest),
//! RSA public-key PEM parsing and RSA-OAEP(SHA-1) encryption.
//!
//! All primitives are one-shot and std-only; nothing here is exposed as a
//! general-purpose crypto API. Vectors in crypto_test_vectors.rs were produced
//! with the device-side Python reference implementation (cuemix_client.py) and
//! independently with Node/.NET, so the OAEP ciphertext is compared byte for
//! byte with the reference implementation for a fixed seed.

use crate::error::CueMixError;

#[cfg(test)]
#[path = "crypto_test_vectors.rs"]
pub(crate) mod test_vectors;

/// WebSocket SHA-1 magic GUID (RFC 6455 section 1.3).
pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// ---------------------------------------------------------------- base64

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 encoding (with padding).
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(B64_ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_ALPHABET[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Standard base64 decoding; ASCII whitespace is ignored, padding is optional.
pub fn base64_decode(text: &str) -> Result<Vec<u8>, CueMixError> {
    let mut sextets = Vec::with_capacity(text.len());
    let mut padding = 0usize;
    for (position, byte) in text.bytes().enumerate() {
        match byte {
            b' ' | b'\t' | b'\r' | b'\n' => {}
            b'=' => padding += 1,
            _ => {
                if padding > 0 {
                    return Err(auth_error(format!(
                        "base64: data after padding at byte {}",
                        position
                    )));
                }
                let value = match byte {
                    b'A'..=b'Z' => byte - b'A',
                    b'a'..=b'z' => byte - b'a' + 26,
                    b'0'..=b'9' => byte - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => {
                        return Err(auth_error(format!(
                            "base64: invalid character 0x{:02x}",
                            byte
                        )))
                    }
                };
                sextets.push(value);
            }
        }
    }
    if padding > 2 {
        return Err(auth_error("base64: too much padding".to_string()));
    }
    if sextets.len() % 4 == 1 {
        return Err(auth_error("base64: truncated group".to_string()));
    }
    let mut out = Vec::with_capacity(sextets.len() / 4 * 3);
    for chunk in sextets.chunks(4) {
        let mut triple: u32 = 0;
        for (i, sextet) in chunk.iter().enumerate() {
            triple |= (*sextet as u32) << (18 - 6 * i);
        }
        out.push((triple >> 16) as u8);
        if chunk.len() > 2 {
            out.push((triple >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(triple as u8);
        }
    }
    Ok(out)
}

fn auth_error(reason: String) -> CueMixError {
    CueMixError::AuthFailed { reason }
}

// ------------------------------------------------------------------- SHA-1

/// SHA-1 digest (20 bytes).
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut blocks = data.chunks_exact(64);
    for block in blocks.by_ref() {
        sha1_compress(&mut state, block);
    }
    let remainder = blocks.remainder();
    let mut tail = [0u8; 128];
    tail[..remainder.len()].copy_from_slice(remainder);
    tail[remainder.len()] = 0x80;
    let tail_len = if remainder.len() < 56 { 64 } else { 128 };
    let bit_len = (data.len() as u64).wrapping_mul(8);
    tail[tail_len - 8..tail_len].copy_from_slice(&bit_len.to_be_bytes());
    for block in tail[..tail_len].chunks_exact(64) {
        sha1_compress(&mut state, block);
    }
    let mut out = [0u8; 20];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn sha1_compress(state: &mut [u32; 5], block: &[u8]) {
    let mut w = [0u32; 80];
    for (i, word) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    for (i, word) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
            20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let tmp = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = tmp;
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
}

/// SHA-1 digest as lowercase hex (test/vector helper).
#[cfg(test)]
pub fn sha1_hex(data: &[u8]) -> String {
    hex(&sha1(data))
}

// ----------------------------------------------------------------- SHA-256

const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// SHA-256 digest (32 bytes).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut blocks = data.chunks_exact(64);
    for block in blocks.by_ref() {
        sha256_compress(&mut state, block);
    }
    let remainder = blocks.remainder();
    let mut tail = [0u8; 128];
    tail[..remainder.len()].copy_from_slice(remainder);
    tail[remainder.len()] = 0x80;
    let tail_len = if remainder.len() < 56 { 64 } else { 128 };
    let bit_len = (data.len() as u64).wrapping_mul(8);
    tail[tail_len - 8..tail_len].copy_from_slice(&bit_len.to_be_bytes());
    for block in tail[..tail_len].chunks_exact(64) {
        sha256_compress(&mut state, block);
    }
    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn sha256_compress(state: &mut [u32; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for (i, word) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
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
    for (i, word) in w.iter().enumerate() {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(SHA256_K[i])
            .wrapping_add(*word);
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

/// SHA-256 digest as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

/// Lowercase hex encoding.
pub fn hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for byte in data {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Parse a lowercase/uppercase hex string into bytes (None when malformed).
#[cfg(test)]
pub fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if !compact.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(compact.len() / 2);
    let bytes = compact.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let high = (bytes[i] as char).to_digit(16)?;
        let low = (bytes[i + 1] as char).to_digit(16)?;
        out.push(((high << 4) | low) as u8);
        i += 2;
    }
    Some(out)
}

// ------------------------------------------------------------- big integers

fn limbs_from_be(bytes: &[u8]) -> Vec<u64> {
    let mut limbs = vec![0u64; bytes.len().div_ceil(8)];
    for (i, byte) in bytes.iter().rev().enumerate() {
        limbs[i / 8] |= (*byte as u64) << ((i % 8) * 8);
    }
    limbs
}

fn limbs_to_be(limbs: &[u64], width: usize) -> Vec<u8> {
    let mut out = vec![0u8; width];
    for (i, limb) in limbs.iter().enumerate() {
        for byte_index in 0..8 {
            let absolute = i * 8 + byte_index;
            if absolute >= width {
                break;
            }
            out[width - 1 - absolute] = (limb >> (byte_index * 8)) as u8;
        }
    }
    out
}

fn trimmed(limbs: &[u64]) -> &[u64] {
    let mut end = limbs.len();
    while end > 0 && limbs[end - 1] == 0 {
        end -= 1;
    }
    &limbs[..end]
}

fn cmp_limbs(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let a = trimmed(a);
    let b = trimmed(b);
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for index in (0..a.len()).rev() {
        if a[index] != b[index] {
            return a[index].cmp(&b[index]);
        }
    }
    std::cmp::Ordering::Equal
}

fn sub_limbs(a: &[u64], b: &[u64]) -> Vec<u64> {
    let len = a.len().max(b.len());
    let mut out = vec![0u64; len];
    let mut borrow = 0u64;
    for (index, slot) in out.iter_mut().enumerate() {
        let av = a.get(index).copied().unwrap_or(0);
        let bv = b.get(index).copied().unwrap_or(0);
        let (diff1, borrow1) = av.overflowing_sub(bv);
        let (diff2, borrow2) = diff1.overflowing_sub(borrow);
        *slot = diff2;
        borrow = u64::from(borrow1 || borrow2);
    }
    out
}

fn add_mod(a: &[u64], b: &[u64], modulus: &[u64]) -> Vec<u64> {
    let len = a.len().max(b.len());
    let mut out = vec![0u64; len + 1];
    let mut carry = 0u64;
    for (index, slot) in out.iter_mut().enumerate().take(len) {
        let av = a.get(index).copied().unwrap_or(0) as u128;
        let bv = b.get(index).copied().unwrap_or(0) as u128;
        let sum = av + bv + carry as u128;
        *slot = sum as u64;
        carry = (sum >> 64) as u64;
    }
    out[len] = carry;
    if cmp_limbs(&out, modulus) != std::cmp::Ordering::Less {
        out = sub_limbs(&out, modulus);
    }
    out
}

/// Montgomery arithmetic context for one odd modulus.
struct Montgomery {
    n: Vec<u64>,
    n0_inv: u64,
    k: usize,
}

impl Montgomery {
    fn new(modulus: &[u64]) -> Option<Self> {
        let n: Vec<u64> = trimmed(modulus).to_vec();
        if n.is_empty() || n[0] & 1 == 0 {
            return None;
        }
        // Newton iteration for n0^{-1} mod 2^64.
        let mut inv = n[0];
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(n[0].wrapping_mul(inv)));
        }
        Some(Self {
            k: n.len(),
            n0_inv: inv.wrapping_neg(),
            n,
        })
    }

    /// R mod n with R = 2^(64k).
    fn r_mod_n(&self) -> Vec<u64> {
        let mut one = vec![0u64; self.k];
        one[0] = 1;
        let mut acc = one;
        for _ in 0..(64 * self.k) {
            acc = add_mod(&acc, &acc, &self.n);
        }
        acc
    }

    /// R^2 mod n (R = 2^(64k)), used to convert into Montgomery form.
    fn r2_mod_n(&self) -> Vec<u64> {
        let mut acc = self.r_mod_n();
        for _ in 0..(64 * self.k) {
            acc = add_mod(&acc, &acc, &self.n);
        }
        acc
    }

    /// a * b * R^-1 mod n. Inputs must be < n and at most k limbs.
    fn mul(&self, a: &[u64], b: &[u64]) -> Vec<u64> {
        let k = self.k;
        let mut t = vec![0u64; 2 * k + 2];
        for i in 0..k {
            let av = a.get(i).copied().unwrap_or(0) as u128;
            let mut carry = 0u128;
            for j in 0..k {
                let index = i + j;
                let product =
                    av * (b.get(j).copied().unwrap_or(0) as u128) + (t[index] as u128) + carry;
                t[index] = product as u64;
                carry = product >> 64;
            }
            let mut index = i + k;
            while carry != 0 {
                let sum = (t[index] as u128) + carry;
                t[index] = sum as u64;
                carry = sum >> 64;
                index += 1;
            }
        }
        for i in 0..k {
            let factor = t[i].wrapping_mul(self.n0_inv);
            let mut carry = 0u128;
            for (j, &n_j) in self.n.iter().take(k).enumerate() {
                let index = i + j;
                let sum = (t[index] as u128) + (factor as u128) * (n_j as u128) + carry;
                t[index] = sum as u64;
                carry = sum >> 64;
            }
            let mut index = i + k;
            while carry != 0 {
                let sum = (t[index] as u128) + carry;
                t[index] = sum as u64;
                carry = sum >> 64;
                index += 1;
            }
        }
        let mut result = t[k..].to_vec();
        let mut guard = 0;
        while cmp_limbs(&result, &self.n) != std::cmp::Ordering::Less {
            result = sub_limbs(&result, &self.n);
            guard += 1;
            if guard > 4 {
                break;
            }
        }
        result
    }
}

/// base^exponent mod modulus over big-endian byte strings; output has the
/// modulus width in bytes. None when the modulus is invalid.
pub fn rsa_modpow(base: &[u8], exponent: &[u8], modulus: &[u8]) -> Option<Vec<u8>> {
    let modulus_limbs = limbs_from_be(modulus);
    let context = Montgomery::new(&modulus_limbs)?;
    let width = modulus_limbs.len() * 8;
    let base_limbs = limbs_from_be(base);
    if cmp_limbs(&base_limbs, &modulus_limbs) != std::cmp::Ordering::Less {
        return None;
    }
    let exponent_limbs = limbs_from_be(exponent);
    let r2 = context.r2_mod_n();
    let r_mod = context.r_mod_n();
    let base_mont = context.mul(&base_limbs, &r2);
    let mut result = r_mod;
    let bits = exponent_limbs.len() * 64;
    for bit in (0..bits).rev() {
        result = context.mul(&result, &result);
        if (exponent_limbs[bit / 64] >> (bit % 64)) & 1 == 1 {
            result = context.mul(&result, &base_mont);
        }
    }
    let one = {
        let mut one = vec![0u64; context.k];
        one[0] = 1;
        one
    };
    let plain = context.mul(&result, &one);
    Some(limbs_to_be(&plain, width))
}

// ------------------------------------------------------------- PEM / DER

/// Parsed RSA public key: big-endian modulus and public exponent, no leading zeros.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RsaPublicKey {
    /// Big-endian modulus bytes (leading zero bytes removed).
    pub modulus_be: Vec<u8>,
    /// Big-endian public exponent bytes (leading zero bytes removed).
    pub exponent_be: Vec<u8>,
}

impl RsaPublicKey {
    /// Modulus width in bytes (also the ciphertext width).
    pub fn modulus_bytes(&self) -> usize {
        self.modulus_be.len()
    }
}

fn der_tlv(buf: &[u8], offset: usize) -> Result<(u8, &[u8], usize), CueMixError> {
    if offset + 2 > buf.len() {
        return Err(auth_error("DER: truncated TLV header".to_string()));
    }
    let tag = buf[offset];
    let first = buf[offset + 1];
    let (length, header) = if first & 0x80 == 0 {
        (first as usize, 2)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 || offset + 2 + count > buf.len() {
            return Err(auth_error("DER: unsupported length encoding".to_string()));
        }
        let mut length = 0usize;
        for byte in &buf[offset + 2..offset + 2 + count] {
            length = (length << 8) | *byte as usize;
        }
        (length, 2 + count)
    };
    let start = offset + header;
    let end = start
        .checked_add(length)
        .ok_or_else(|| auth_error("DER: length overflow".to_string()))?;
    if end > buf.len() {
        return Err(auth_error("DER: value exceeds buffer".to_string()));
    }
    Ok((tag, &buf[start..end], end))
}

fn der_integer(content: &[u8]) -> Result<Vec<u8>, CueMixError> {
    let mut value = content.to_vec();
    while value.len() > 1 && value[0] == 0 {
        value.remove(0);
    }
    if value.is_empty() {
        return Err(auth_error("DER: empty integer".to_string()));
    }
    Ok(value)
}

fn rsa_key_from_integers(content: &[u8]) -> Result<RsaPublicKey, CueMixError> {
    let (n_tag, n_bytes, offset) = der_tlv(content, 0)?;
    if n_tag != 0x02 {
        return Err(auth_error("DER: expected INTEGER modulus".to_string()));
    }
    let (e_tag, e_bytes, _) = der_tlv(content, offset)?;
    if e_tag != 0x02 {
        return Err(auth_error("DER: expected INTEGER exponent".to_string()));
    }
    let modulus_be = der_integer(n_bytes)?;
    let exponent_be = der_integer(e_bytes)?;
    if modulus_be.len() < 32 {
        return Err(auth_error(
            "RSA key is too short to be the device key".to_string(),
        ));
    }
    Ok(RsaPublicKey {
        modulus_be,
        exponent_be,
    })
}

/// Parse an RSA public key from PEM: accepts both SPKI ("BEGIN PUBLIC KEY") and
/// PKCS#1 ("BEGIN RSA PUBLIC KEY") encodings, and any PEM label spelling.
pub fn parse_rsa_public_key_pem(pem: &str) -> Result<RsaPublicKey, CueMixError> {
    let mut body = String::new();
    for line in pem.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("-----") {
            continue;
        }
        body.push_str(trimmed);
    }
    if body.is_empty() {
        return Err(auth_error("PEM: empty key body".to_string()));
    }
    let der = base64_decode(&body)?;
    let (tag, content, _) = der_tlv(&der, 0)?;
    if tag != 0x30 {
        return Err(auth_error("DER: top level is not a SEQUENCE".to_string()));
    }
    if content.is_empty() {
        return Err(auth_error("DER: empty top level SEQUENCE".to_string()));
    }
    match content[0] {
        // PKCS#1 RSAPublicKey: SEQUENCE { INTEGER n, INTEGER e }.
        0x02 => rsa_key_from_integers(content),
        // SubjectPublicKeyInfo: SEQUENCE { AlgorithmIdentifier, BIT STRING }.
        0x30 => {
            let (alg_tag, _, bit_offset) = der_tlv(content, 0)?;
            if alg_tag != 0x30 {
                return Err(auth_error("DER: expected AlgorithmIdentifier".to_string()));
            }
            let (bit_tag, bit_string, _) = der_tlv(content, bit_offset)?;
            if bit_tag != 0x03 || bit_string.is_empty() {
                return Err(auth_error("DER: expected BIT STRING".to_string()));
            }
            if bit_string[0] != 0 {
                return Err(auth_error("DER: unexpected unused bits in key".to_string()));
            }
            let (key_tag, key_content, _) = der_tlv(&bit_string[1..], 0)?;
            if key_tag != 0x30 {
                return Err(auth_error(
                    "DER: expected RSAPublicKey SEQUENCE".to_string(),
                ));
            }
            rsa_key_from_integers(key_content)
        }
        _ => Err(auth_error(
            "DER: unsupported RSA public key encoding".to_string(),
        )),
    }
}

// ------------------------------------------------------------------- OAEP

/// MGF1 based on SHA-1 (RFC 8017 B.2.1).
pub fn mgf1_sha1(seed: &[u8], length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length + 20);
    let mut counter: u32 = 0;
    while out.len() < length {
        let mut input = Vec::with_capacity(seed.len() + 4);
        input.extend_from_slice(seed);
        input.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&sha1(&input));
        counter = counter.wrapping_add(1);
    }
    out.truncate(length);
    out
}

/// RSA-OAEP(SHA-1) encryption with an explicit 20-byte seed (deterministic; for
/// tests and cross-implementation vectors).
pub fn oaep_sha1_encrypt_with_seed(
    pem: &str,
    message: &[u8],
    seed: &[u8; 20],
) -> Result<Vec<u8>, CueMixError> {
    let key = parse_rsa_public_key_pem(pem)?;
    oaep_sha1_encrypt_with_key(&key, message, seed)
}

/// RSA-OAEP(SHA-1) encryption of message with a parsed public key.
pub fn oaep_sha1_encrypt_with_key(
    key: &RsaPublicKey,
    message: &[u8],
    seed: &[u8; 20],
) -> Result<Vec<u8>, CueMixError> {
    let hlen = 20usize;
    let k = key.modulus_bytes();
    if k < 2 * hlen + 2 {
        return Err(auth_error("RSA key too short for OAEP".to_string()));
    }
    let max_message = k - 2 * hlen - 2;
    if message.len() > max_message {
        return Err(auth_error(format!(
            "OAEP message too long ({} > {})",
            message.len(),
            max_message
        )));
    }
    let label_hash = sha1(b"");
    let mut db = Vec::with_capacity(k - hlen - 1);
    db.extend_from_slice(&label_hash);
    db.extend(std::iter::repeat_n(0u8, max_message - message.len()));
    db.push(0x01);
    db.extend_from_slice(message);
    let masked_db = xor_bytes(&db, &mgf1_sha1(seed, k - hlen - 1));
    let masked_seed = xor_bytes(seed, &mgf1_sha1(&masked_db, hlen));
    let mut encoded = Vec::with_capacity(k);
    encoded.push(0);
    encoded.extend_from_slice(&masked_seed);
    encoded.extend_from_slice(&masked_db);
    rsa_modpow(&encoded, &key.exponent_be, &key.modulus_be)
        .ok_or_else(|| auth_error("RSA modular exponentiation failed".to_string()))
}

fn xor_bytes(left: &[u8], right: &[u8]) -> Vec<u8> {
    left.iter().zip(right).map(|(a, b)| a ^ b).collect()
}

/// Random-seed RSA-OAEP(SHA-1) encryption (the real auth path).
pub fn oaep_sha1_encrypt(pem: &str, message: &[u8]) -> Result<Vec<u8>, CueMixError> {
    let mut seed = [0u8; 20];
    crate::rng::fill_random(&mut seed);
    oaep_sha1_encrypt_with_seed(pem, message, &seed)
}

/// Password digest sent to the device: lowercase SHA-256 hex of the UTF-8 password.
pub fn password_digest_hex(password: &str) -> String {
    sha256_hex(password.as_bytes())
}

/// Build the RSA-OAEP(SHA-1) auth response payload for a device public key PEM.
pub fn encrypt_password(pem: &str, password: &str) -> Result<Vec<u8>, CueMixError> {
    oaep_sha1_encrypt(pem, password_digest_hex(password).as_bytes())
}

// ------------------------------------------------------------- WS accept key

/// Sec-WebSocket-Accept for a Sec-WebSocket-Key (base64(SHA-1(key + GUID))).
pub fn websocket_accept_key(sec_websocket_key: &str) -> String {
    let mut data = Vec::with_capacity(sec_websocket_key.len() + WS_GUID.len());
    data.extend_from_slice(sec_websocket_key.as_bytes());
    data.extend_from_slice(WS_GUID.as_bytes());
    base64_encode(&sha1(&data))
}

#[cfg(test)]
mod tests {
    use super::test_vectors as v;
    use super::*;

    fn hex_to_bytes(text: &str) -> Vec<u8> {
        // Vectors are big-endian integers; Node BigInt toString(16) drops a
        // leading zero nibble, so pad odd lengths before byte conversion.
        let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let padded = if compact.len() % 2 == 1 {
            format!("0{}", compact)
        } else {
            compact
        };
        hex_decode(&padded).expect("valid hex vector")
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        for (text, _, _, b64) in v::HASH_VECTORS {
            assert_eq!(base64_encode(text.as_bytes()), *b64);
        }
    }

    #[test]
    fn base64_decodes_padding_and_whitespace() {
        assert_eq!(base64_decode("Zm9vYmFy").expect("decode"), b"foobar");
        assert_eq!(base64_decode(" Zm9v\nYmFy\r\n").expect("decode"), b"foobar");
        assert_eq!(base64_decode("Zg").expect("unpadded"), b"f");
        assert!(base64_decode("Z").is_err());
        assert!(base64_decode("Zg==Zg==").is_err());
        assert!(base64_decode("Zm9v!").is_err());
        for (text, _, _, b64) in v::HASH_VECTORS {
            assert_eq!(base64_decode(b64).expect("decode vector"), text.as_bytes());
        }
    }

    #[test]
    fn sha1_matches_reference_hashes() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"The quick brown fox jumps over the lazy dog"),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
        for (text, want, _, _) in v::HASH_VECTORS {
            assert_eq!(sha1_hex(text.as_bytes()), *want, "sha1 {:?}", text);
        }
    }

    #[test]
    fn sha256_matches_reference_hashes() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        for (text, _, want, _) in v::HASH_VECTORS {
            assert_eq!(sha256_hex(text.as_bytes()), *want, "sha256 {:?}", text);
        }
    }

    #[test]
    fn websocket_accept_matches_rfc6455_example() {
        assert_eq!(
            websocket_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(hex(b"\x00\x01\xfe"), "0001fe");
        assert_eq!(
            hex_decode("00 01 FE").expect("decode"),
            vec![0x00, 0x01, 0xfe]
        );
        assert_eq!(hex_decode("0g"), None);
        assert_eq!(hex_decode("abc"), None);
    }

    #[test]
    fn parses_spki_pem_of_2048_bit_key() {
        let key = parse_rsa_public_key_pem(v::RSA2048_PEM_SPKI).expect("parse spki");
        assert_eq!(key.modulus_be, hex_to_bytes(v::RSA2048_N_HEX));
        assert_eq!(key.exponent_be, hex_to_bytes(v::RSA2048_E_HEX));
        assert_eq!(key.modulus_bytes(), 256);
    }

    #[test]
    fn parses_pkcs1_and_spki_pem_of_1024_bit_key() {
        let spki = parse_rsa_public_key_pem(v::RSA1024_PEM_SPKI).expect("parse spki 1024");
        let pkcs1 = parse_rsa_public_key_pem(v::RSA1024_PEM_PKCS1).expect("parse pkcs1 1024");
        assert_eq!(spki, pkcs1);
        assert_eq!(spki.modulus_be, hex_to_bytes(v::RSA1024_N_HEX));
        assert_eq!(spki.exponent_be, hex_to_bytes(v::RSA1024_E_HEX));
    }

    #[test]
    fn rejects_garbage_pem() {
        assert!(parse_rsa_public_key_pem("").is_err());
        assert!(parse_rsa_public_key_pem(
            "-----BEGIN PUBLIC KEY-----\n!\n-----END PUBLIC KEY-----"
        )
        .is_err());
        assert!(parse_rsa_public_key_pem("MAA=").is_err());
    }

    #[test]
    fn modpow_matches_node_vectors() {
        let c1 = rsa_modpow(
            &hex_to_bytes(v::MODEXP_M1_HEX),
            &hex_to_bytes(v::RSA2048_E_HEX),
            &hex_to_bytes(v::RSA2048_N_HEX),
        )
        .expect("modpow 1");
        assert_eq!(c1, hex_to_bytes(v::MODEXP_C1_HEX), "modexp vector 1");
        let c2 = rsa_modpow(
            &hex_to_bytes(v::MODEXP_M2_HEX),
            &hex_to_bytes(v::RSA2048_E_HEX),
            &hex_to_bytes(v::RSA2048_N_HEX),
        )
        .expect("modpow 2");
        assert_eq!(c2, hex_to_bytes(v::MODEXP_C2_HEX), "modexp vector 2");
    }

    #[test]
    fn modpow_rejects_invalid_inputs() {
        let n = hex_to_bytes(v::RSA2048_N_HEX);
        assert!(rsa_modpow(&[1], &[1], &[]).is_none(), "empty modulus");
        assert!(rsa_modpow(&[1], &[1], &[2, 0]).is_none(), "even modulus");
        let huge = vec![0xff; 300];
        assert!(rsa_modpow(&huge, &[1], &n).is_none(), "base >= modulus");
    }

    /// OAEP-SHA1 decryption using the raw private exponent (test-only).
    fn oaep_decrypt(key: &RsaPublicKey, d_be: &[u8], cipher: &[u8]) -> Vec<u8> {
        let k = key.modulus_bytes();
        let mut em = rsa_modpow(cipher, d_be, &key.modulus_be).expect("private modpow");
        assert_eq!(em.len(), k);
        assert_eq!(em[0], 0, "OAEP encoded message must start with 0x00");
        em.remove(0);
        // Careful: after removing the leading zero, k -> k-1 bytes; split masked parts.
        let hlen = 20usize;
        let masked_seed = em[..hlen].to_vec();
        let masked_db = em[hlen..].to_vec();
        let seed = xor_bytes(&masked_seed, &mgf1_sha1(&masked_db, hlen));
        let db = xor_bytes(&masked_db, &mgf1_sha1(&seed, k - hlen - 1));
        assert_eq!(&db[..hlen], &sha1(b"")[..], "OAEP label hash mismatch");
        let mut index = hlen;
        while db[index] == 0 {
            index += 1;
        }
        assert_eq!(db[index], 0x01, "OAEP separator missing");
        db[index + 1..].to_vec()
    }

    #[test]
    fn oaep_matches_reference_implementation_byte_for_byte() {
        let key = parse_rsa_public_key_pem(v::RSA2048_PEM_SPKI).expect("key");
        let mut seed = [0u8; 20];
        seed.copy_from_slice(&hex_to_bytes(v::OAEP_SEED_HEX));
        let message = hex_to_bytes(v::OAEP_MSG_HEX);
        let cipher = oaep_sha1_encrypt_with_key(&key, &message, &seed).expect("encrypt");
        assert_eq!(
            hex(&cipher),
            v::OAEP_CIPHER_HEX.to_lowercase(),
            "OAEP ciphertext must match the reference implementation"
        );
        let decrypted = oaep_decrypt(&key, &hex_to_bytes(v::RSA2048_D_HEX), &cipher);
        assert_eq!(decrypted, message);
    }

    #[test]
    fn oaep_empty_message_matches_reference() {
        let key = parse_rsa_public_key_pem(v::RSA2048_PEM_SPKI).expect("key");
        let mut seed = [0u8; 20];
        seed.copy_from_slice(&hex_to_bytes(v::OAEP_SEED_HEX));
        let cipher = oaep_sha1_encrypt_with_key(&key, &[], &seed).expect("encrypt");
        assert_eq!(hex(&cipher), v::OAEP_EMPTY_CIPHER_HEX.to_lowercase());
    }

    #[test]
    fn encrypt_password_roundtrips_through_oaep_decrypt() {
        let key = parse_rsa_public_key_pem(v::RSA2048_PEM_SPKI).expect("key");
        let cipher =
            encrypt_password(v::RSA2048_PEM_SPKI, "secret-pass").expect("encrypt password");
        let recovered = oaep_decrypt(&key, &hex_to_bytes(v::RSA2048_D_HEX), &cipher);
        assert_eq!(recovered, password_digest_hex("secret-pass").as_bytes());
    }

    #[test]
    fn password_digest_is_sha256_hex() {
        assert_eq!(
            password_digest_hex("khaen-test-password"),
            "129218be08eeef9420ee738c05bc8d0587f3a987d74cc0d825bab4264116b7f1"
        );
        assert_eq!(
            password_digest_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn oaep_rejects_too_long_message() {
        let key = parse_rsa_public_key_pem(v::RSA2048_PEM_SPKI).expect("key");
        let mut seed = [0u8; 20];
        seed.fill(7);
        let too_long = vec![0u8; 215];
        assert!(oaep_sha1_encrypt_with_key(&key, &too_long, &seed).is_err());
    }

    #[test]
    fn mgf1_has_requested_length_and_is_seed_sensitive() {
        let a = mgf1_sha1(b"seed-a", 33);
        let b = mgf1_sha1(b"seed-b", 33);
        assert_eq!(a.len(), 33);
        assert_eq!(b.len(), 33);
        assert_ne!(a, b);
        assert_eq!(mgf1_sha1(b"x", 0).len(), 0);
    }
}
