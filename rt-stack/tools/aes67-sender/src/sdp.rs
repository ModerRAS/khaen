//! AES67-style SDP generation for the sender.
//!
//! For the Windows first stage there is no PTP/PHC, so the advertised clock
//! reference is `a=ts-refclk:local` (not an IEEE1588 PTP reference) and
//! `a=mediaclk:direct=0`. This is intentionally *not* strict AES67 PTP sync;
//! it is enough for link/content verification. See CLOCK-AND-SYNC.md.

use anyhow::{bail, Result};

#[derive(Debug, Clone)]
pub struct SdpConfig {
    pub session_name: String,
    pub origin_ip: String,
    pub dest_ip: String,
    pub multicast: bool,
    pub port: u16,
    pub payload_type: u8,
    pub channels: u8,
    pub ptime_ms: f64,
    /// e.g. `local` or `ptp=IEEE1588-2008:00-11-22-33-44-55:0`
    pub ts_refclk: String,
    pub ssrc: u32,
}

impl SdpConfig {
    pub fn render(&self) -> Result<String> {
        if self.channels == 0 || self.channels > 8 {
            bail!("AES67 flow supports 1..=8 channels, got {}", self.channels);
        }
        if self.ptime_ms <= 0.0 {
            bail!("ptime must be > 0");
        }
        let conn = if self.multicast {
            format!("c=IN IP4 {}/32", self.dest_ip)
        } else {
            format!("c=IN IP4 {}", self.dest_ip)
        };
        let ptime = fmt_ptime(self.ptime_ms);
        let mut s = String::new();
        s.push_str("v=0\n");
        s.push_str(&format!(
            "o=- {} 1 IN IP4 {}\n",
            session_id(self.ssrc),
            self.origin_ip
        ));
        s.push_str(&format!("s={}\n", self.session_name));
        s.push_str(&format!("{conn}\n"));
        s.push_str("t=0 0\n");
        s.push_str(&format!(
            "m=audio {} RTP/AVP {}\n",
            self.port, self.payload_type
        ));
        s.push_str(&format!(
            "a=rtpmap:{} L24/48000/{}\n",
            self.payload_type, self.channels
        ));
        s.push_str(&format!("a=ptime:{ptime}\n"));
        s.push_str(&format!("a=ts-refclk:{}\n", self.ts_refclk));
        s.push_str("a=mediaclk:direct=0\n");
        s.push_str(&format!(
            "a=ssrc:{} cname:tspi-aes67-sender@{}\n",
            self.ssrc, self.origin_ip
        ));
        Ok(s)
    }
}

fn session_id(ssrc: u32) -> u64 {
    // Deterministic 64-bit session id derived from SSRC (stable per run/flags).
    let hi = (ssrc as u64) << 24;
    hi | 0x51_0000
}

fn fmt_ptime(ms: f64) -> String {
    if (ms.fract()).abs() < 1e-9 {
        format!("{}", ms as i64)
    } else {
        let mut s = format!("{:.3}", ms);
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SdpConfig {
        SdpConfig {
            session_name: "TSPI Windows AES67 sender".to_string(),
            origin_ip: "192.0.2.94".to_string(),
            dest_ip: "239.69.63.10".to_string(),
            multicast: true,
            port: 5004,
            payload_type: 97,
            channels: 2,
            ptime_ms: 1.0,
            ts_refclk: "local".to_string(),
            ssrc: 0x0102_0304,
        }
    }

    #[test]
    fn renders_multicast_sdp() {
        let s = cfg().render().unwrap();
        assert!(s.starts_with("v=0\n"));
        assert!(s.contains("c=IN IP4 239.69.63.10/32\n"));
        assert!(s.contains("m=audio 5004 RTP/AVP 97\n"));
        assert!(s.contains("a=rtpmap:97 L24/48000/2\n"));
        assert!(s.contains("a=ptime:1\n"));
        assert!(s.contains("a=ts-refclk:local\n"));
        assert!(s.contains("a=mediaclk:direct=0\n"));
        assert!(s.contains("a=ssrc:16909060 cname:tspi-aes67-sender@192.0.2.94\n"));
    }

    #[test]
    fn renders_unicast_and_fractional_ptime() {
        let mut c = cfg();
        c.multicast = false;
        c.dest_ip = "127.0.0.1".to_string();
        c.ptime_ms = 0.125;
        let s = c.render().unwrap();
        assert!(s.contains("c=IN IP4 127.0.0.1\n"));
        assert!(s.contains("a=ptime:0.125\n"));
    }

    #[test]
    fn rejects_bad_channels() {
        let mut c = cfg();
        c.channels = 9;
        assert!(c.render().is_err());
    }
}
