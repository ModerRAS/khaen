//! CRC-32C（Castagnoli），用于方案文件各区段与整体的完整性校验。

const POLY: u32 = 0x82F6_3B78;

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static TABLE: [u32; 256] = make_table();

/// 计算 CRC-32C。标准反射实现：初值 0xFFFF_FFFF，结果取反。
pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        let idx = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[idx];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::crc32c;

    /// RFC 3720 附录 B 的标准校验值。
    #[test]
    fn known_vectors() {
        assert_eq!(crc32c(b""), 0x0000_0000);
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b"a"), 0xC1D0_4330);
    }

    #[test]
    fn differs_on_single_bit_flip() {
        let a = crc32c(&[0u8; 64]);
        let mut b = [0u8; 64];
        b[17] = 1;
        assert_ne!(a, crc32c(&b));
    }
}
