//! Device detection for the UltraLite-mk5 control path.
//!
//! Linux: parse /proc/asound/cards for the MOTU card (path injectable) and probe
//! the USB-CDC control network with a 1 s TCP connect. Windows / no device:
//! detected = false, no error. Parsing and probing are injectable so the logic
//! is testable everywhere.

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

/// Default /proc/asound/cards path on Linux.
pub const DEFAULT_CARDS_PATH: &str = "/proc/asound/cards";
/// Default control endpoint (device side of the USB-CDC link).
pub const DEFAULT_CONTROL_ADDR: &str = "169.254.111.126:1280";
/// Default control-network probe timeout.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(1000);

/// Detection succeeded: MOTU card found and control network reachable.
pub const REASON_DETECTED: &str = "detected";
/// No MOTU card in /proc/asound/cards.
pub const REASON_NO_MOTU_CARD: &str = "no_motu_card";
/// /proc/asound/cards is not readable (e.g. Windows); not an error.
pub const REASON_CARDS_UNAVAILABLE: &str = "cards_unavailable";
/// MOTU card present but the control network TCP probe failed.
pub const REASON_CONTROL_UNREACHABLE: &str = "control_unreachable";

/// Detection options.
#[derive(Clone, Debug)]
pub struct DetectConfig {
    /// Path of the ALSA cards file (injectable for tests).
    pub cards_path: PathBuf,
    /// Control network endpoint to probe.
    pub control_addr: SocketAddr,
    /// Probe timeout.
    pub probe_timeout: Duration,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            cards_path: PathBuf::from(DEFAULT_CARDS_PATH),
            control_addr: DEFAULT_CONTROL_ADDR
                .parse()
                .unwrap_or_else(|_| SocketAddr::from(([169, 254, 111, 126], 1280))),
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
        }
    }
}

/// One card entry of /proc/asound/cards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundCard {
    /// Card index (the number in " 0 [UltraLitemk5 ]").
    pub index: u32,
    /// ALSA card id inside the brackets (e.g. "UltraLitemk5").
    pub id: String,
    /// Driver/name part after " - " on the card line (e.g. "UltraLite-mk5").
    pub name: String,
    /// Continuation line with the USB path (e.g. "MOTU UltraLite-mk5 at usb-...").
    pub detail: String,
}

impl SoundCard {
    /// Best-effort human model name: the name after " - ", else the detail line.
    pub fn model(&self) -> Option<String> {
        if !self.name.trim().is_empty() {
            return Some(self.name.trim().to_string());
        }
        let detail = self.detail.trim();
        if detail.is_empty() {
            return None;
        }
        let without_vendor = detail.strip_prefix("MOTU ").unwrap_or(detail);
        Some(
            without_vendor
                .split(" at ")
                .next()
                .unwrap_or(without_vendor)
                .trim()
                .to_string(),
        )
    }

    /// True when any field identifies a MOTU UltraLite.
    pub fn is_motu_ultralite(&self) -> bool {
        let haystacks = [self.id.as_str(), self.name.as_str(), self.detail.as_str()];
        haystacks.iter().any(|haystack| {
            let lowered = haystack.to_ascii_lowercase();
            lowered.contains("ultralite") || lowered.contains("motu")
        })
    }
}

/// Result of a device detection attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceDetection {
    /// A MOTU card was found by ALSA.
    pub detected: bool,
    /// ALSA card id (e.g. "UltraLitemk5").
    pub card_id: Option<String>,
    /// ALSA card index (e.g. 0).
    pub card_index: Option<u32>,
    /// Model name (e.g. "UltraLite-mk5").
    pub model: Option<String>,
    /// Control network TCP probe succeeded.
    pub control_reachable: bool,
    /// Stable reason code (REASON_* constants).
    pub reason: &'static str,
}

impl DeviceDetection {
    /// Device is detected and its control network is reachable.
    pub fn is_ready(&self) -> bool {
        self.detected && self.control_reachable
    }

    fn absent(reason: &'static str) -> Self {
        Self {
            detected: false,
            card_id: None,
            card_index: None,
            model: None,
            control_reachable: false,
            reason,
        }
    }

    fn from_card(card: &SoundCard, control_reachable: bool) -> Self {
        Self {
            detected: true,
            card_id: Some(card.id.clone()),
            card_index: Some(card.index),
            model: card.model(),
            control_reachable,
            reason: if control_reachable {
                REASON_DETECTED
            } else {
                REASON_CONTROL_UNREACHABLE
            },
        }
    }
}

/// Parse /proc/asound/cards text into card entries.
pub fn parse_asound_cards(text: &str) -> Vec<SoundCard> {
    let mut cards: Vec<SoundCard> = Vec::new();
    for line in text.lines() {
        let trimmed_start = line.trim_start();
        let starts_with_digit = trimmed_start
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false);
        if starts_with_digit && trimmed_start.contains('[') {
            let Some(open) = trimmed_start.find('[') else {
                continue;
            };
            let Some(close) = trimmed_start[open..].find(']') else {
                continue;
            };
            let index_text = trimmed_start[..open].trim();
            let Ok(index) = index_text.parse::<u32>() else {
                continue;
            };
            let id = trimmed_start[open + 1..open + close].trim().to_string();
            let after = trimmed_start[open + close + 1..].trim_start();
            let after = after.strip_prefix(':').unwrap_or(after).trim();
            let name = after
                .split_once(" - ")
                .map(|(_, name)| name.trim().to_string())
                .unwrap_or_default();
            cards.push(SoundCard {
                index,
                id,
                name,
                detail: String::new(),
            });
        } else if let Some(card) = cards.last_mut() {
            let detail = trimmed_start.trim();
            if !detail.is_empty() {
                if !card.detail.is_empty() {
                    card.detail.push(' ');
                }
                card.detail.push_str(detail);
            }
        }
    }
    cards
}

/// Find the MOTU UltraLite card entry.
pub fn find_motu(cards: &[SoundCard]) -> Option<&SoundCard> {
    cards.iter().find(|card| card.is_motu_ultralite())
}

fn tcp_probe(addr: SocketAddr, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

/// Detect the device with an injectable cards text and control probe.
///
/// When cards_text is Some it is parsed instead of reading config.cards_path;
/// the probe closure is called only when a MOTU card was found.
pub fn detect_with(
    config: &DetectConfig,
    cards_text: Option<&str>,
    probe: &dyn Fn(SocketAddr, Duration) -> bool,
) -> DeviceDetection {
    let owned;
    let text: Option<&str> = match cards_text {
        Some(text) => Some(text),
        None => match std::fs::read_to_string(&config.cards_path) {
            Ok(text) => {
                owned = text;
                Some(&owned)
            }
            Err(_) => None,
        },
    };
    let Some(text) = text else {
        return DeviceDetection::absent(REASON_CARDS_UNAVAILABLE);
    };
    let cards = parse_asound_cards(text);
    let Some(card) = find_motu(&cards) else {
        return DeviceDetection::absent(REASON_NO_MOTU_CARD);
    };
    let reachable = probe(config.control_addr, config.probe_timeout);
    DeviceDetection::from_card(card, reachable)
}

/// Detect the device: read the cards file and probe the control network.
pub fn detect(config: &DetectConfig) -> DeviceDetection {
    detect_with(config, None, &tcp_probe)
}

/// Detect from an explicit cards text (convenience for tools/tests).
pub fn detect_from_text(config: &DetectConfig, cards_text: &str) -> DeviceDetection {
    detect_with(config, Some(cards_text), &tcp_probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Card layout observed on N150 (docs/CLOSED-LOOP.md).
    const N150_CARDS: &str = " 0 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5\n                      MOTU UltraLite-mk5 at usb-0000:00:14.0-2.1, high speed\n 1 [U18dB          ]: USB-Audio - Umik-1  Gain: 18dB\n                      miniDSP Umik-1 at usb-0000:00:14.0-3, full speed\n 2 [HID            ]: USB-Audio - HID\n 3 [PCH            ]: HDA-Intel - HDA Intel PCH\n                      HDA Intel PCH at 0xf7f10000 irq 125\n 4 [RAVENNA        ]: USB-Audio - RAVENNA\n";

    fn probe_true(_: SocketAddr, _: Duration) -> bool {
        true
    }

    fn probe_false(_: SocketAddr, _: Duration) -> bool {
        false
    }

    fn config() -> DetectConfig {
        DetectConfig::default()
    }

    #[test]
    fn parses_n150_cards_text() {
        let cards = parse_asound_cards(N150_CARDS);
        assert_eq!(cards.len(), 5);
        assert_eq!(cards[0].index, 0);
        assert_eq!(cards[0].id, "UltraLitemk5");
        assert_eq!(cards[0].name, "UltraLite-mk5");
        assert!(cards[0].detail.contains("MOTU UltraLite-mk5 at usb-"));
        assert_eq!(cards[1].id, "U18dB");
        assert_eq!(cards[4].id, "RAVENNA");
        assert_eq!(cards[0].model().as_deref(), Some("UltraLite-mk5"));
    }

    #[test]
    fn finds_motu_and_reports_ready_state() {
        let cards = parse_asound_cards(N150_CARDS);
        let motu = find_motu(&cards).expect("motu card");
        assert_eq!(motu.index, 0);
        let detection = detect_with(&config(), Some(N150_CARDS), &probe_true);
        assert!(detection.detected);
        assert!(detection.control_reachable);
        assert!(detection.is_ready());
        assert_eq!(detection.card_id.as_deref(), Some("UltraLitemk5"));
        assert_eq!(detection.card_index, Some(0));
        assert_eq!(detection.model.as_deref(), Some("UltraLite-mk5"));
        assert_eq!(detection.reason, REASON_DETECTED);
    }

    #[test]
    fn unreachable_control_network_is_not_ready() {
        let detection = detect_with(&config(), Some(N150_CARDS), &probe_false);
        assert!(detection.detected);
        assert!(!detection.control_reachable);
        assert!(!detection.is_ready());
        assert_eq!(detection.reason, REASON_CONTROL_UNREACHABLE);
    }

    #[test]
    fn no_motu_card_is_reported_without_err() {
        let cards = " 3 [PCH            ]: HDA-Intel - HDA Intel PCH\n 1 [U18dB          ]: USB-Audio - Umik-1  Gain: 18dB\n";
        let detection = detect_with(&config(), Some(cards), &probe_true);
        assert!(!detection.detected);
        assert_eq!(detection.reason, REASON_NO_MOTU_CARD);
        assert_eq!(detection.card_id, None);
        assert!(!detection.control_reachable);
    }

    #[test]
    fn missing_cards_file_is_not_an_error() {
        let mut cfg = config();
        cfg.cards_path = PathBuf::from("C:/definitely/not/here/asound-cards");
        // probe must not be called when there is no cards file.
        let probe = |_: SocketAddr, _: Duration| -> bool { panic!("probe must not run") };
        let detection = detect_with(&cfg, None, &probe);
        assert!(!detection.detected);
        assert_eq!(detection.reason, REASON_CARDS_UNAVAILABLE);
    }

    #[test]
    fn probe_receives_configured_address_and_timeout() {
        let mut cfg = config();
        cfg.control_addr = SocketAddr::from(([127, 0, 0, 1], 4321));
        cfg.probe_timeout = Duration::from_millis(77);
        let seen = std::cell::RefCell::new(Vec::new());
        let probe = |addr: SocketAddr, timeout: Duration| -> bool {
            seen.borrow_mut().push((addr, timeout));
            true
        };
        let detection = detect_with(&cfg, Some(N150_CARDS), &probe);
        assert!(detection.control_reachable);
        assert_eq!(*seen.borrow(), vec![(cfg.control_addr, cfg.probe_timeout)]);
    }

    #[test]
    fn tolerates_odd_cards_text() {
        assert!(parse_asound_cards("").is_empty());
        assert!(parse_asound_cards("garbage line\n[not a card]\n").is_empty());
        let one = parse_asound_cards(" 7 [UltraLite-mk5  ]: USB-Audio - UltraLite-mk5\n");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].model().as_deref(), Some("UltraLite-mk5"));
    }

    #[test]
    fn model_falls_back_to_detail_line() {
        let cards = parse_asound_cards(" 2 [MOTU           ]: USB-Audio\n                      MOTU UltraLite-mk5 at usb-0000:00:14.0-2, high speed\n");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].model().as_deref(), Some("UltraLite-mk5"));
    }
}
