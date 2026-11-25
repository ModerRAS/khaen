//! Tiny hand-rolled CLI parser (keeps the dependency set minimal and offline).

use anyhow::{bail, Result};

#[derive(Debug, Clone, Default)]
pub struct Args {
    items: Vec<(String, String)>,
    pub help: bool,
}

impl Args {
    pub fn from_env() -> Result<Self> {
        Self::parse(std::env::args().skip(1))
    }

    pub fn parse<I: IntoIterator<Item = String>>(iter: I) -> Result<Self> {
        let mut items: Vec<(String, String)> = Vec::new();
        let mut help = false;
        let mut it = iter.into_iter().peekable();
        while let Some(tok) = it.next() {
            if tok == "--help" || tok == "-h" {
                help = true;
                continue;
            }
            let (key, inline) = match tok.split_once('=') {
                Some((k, v)) if k.starts_with("--") => (k.to_string(), Some(v.to_string())),
                _ => (tok.clone(), None),
            };
            if !key.starts_with("--") {
                bail!(
                    "unexpected positional argument {tok:?} (this tool is flag-only; use --help)"
                );
            }
            let key = key.trim_start_matches("--").to_string();
            let value = if let Some(v) = inline {
                v
            } else if it.peek().map(|p| !p.starts_with("--")).unwrap_or(false) {
                it.next().unwrap()
            } else {
                String::new()
            };
            items.push((key, value));
        }
        Ok(Self { items, help })
    }

    pub fn has(&self, key: &str) -> bool {
        self.items.iter().any(|(k, _)| k == key)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.items
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn get_all(&self, key: &str) -> Vec<&str> {
        self.items
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn require(&self, key: &str) -> Result<&str> {
        match self.get(key) {
            Some(v) if !v.is_empty() => Ok(v),
            _ => bail!("missing required flag --{key}"),
        }
    }

    pub fn string(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or(default).to_string()
    }

    pub fn opt_string(&self, key: &str) -> Option<String> {
        self.get(key).map(|s| s.to_string())
    }

    pub fn f64_opt(&self, key: &str) -> Result<Option<f64>> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => Ok(Some(v.parse::<f64>().map_err(|_| {
                anyhow::anyhow!("--{key} expects a number, got {v:?}")
            })?)),
        }
    }

    pub fn f64_or(&self, key: &str, default: f64) -> Result<f64> {
        Ok(self.f64_opt(key)?.unwrap_or(default))
    }

    pub fn u64_opt(&self, key: &str) -> Result<Option<u64>> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => {
                Ok(Some(v.parse::<u64>().map_err(|_| {
                    anyhow::anyhow!("--{key} expects a u64, got {v:?}")
                })?))
            }
        }
    }

    pub fn u64_or(&self, key: &str, default: u64) -> Result<u64> {
        Ok(self.u64_opt(key)?.unwrap_or(default))
    }

    pub fn u16_opt(&self, key: &str) -> Result<Option<u16>> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => {
                Ok(Some(v.parse::<u16>().map_err(|_| {
                    anyhow::anyhow!("--{key} expects a u16, got {v:?}")
                })?))
            }
        }
    }

    pub fn u16_or(&self, key: &str, default: u16) -> Result<u16> {
        Ok(self.u16_opt(key)?.unwrap_or(default))
    }

    pub fn usize_or(&self, key: &str, default: usize) -> Result<usize> {
        match self.get(key) {
            None => Ok(default),
            Some(v) => v
                .parse::<usize>()
                .map_err(|_| anyhow::anyhow!("--{key} expects a usize, got {v:?}")),
        }
    }

    pub fn bool_flag(&self, key: &str) -> bool {
        self.has(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> Args {
        Args::parse(v.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn parses_flags_and_values() {
        let a = parse(&[
            "--source",
            "wav",
            "--duration",
            "3.5",
            "--loop",
            "--ssrc=1234",
        ]);
        assert_eq!(a.get("source"), Some("wav"));
        assert_eq!(a.f64_opt("duration").unwrap(), Some(3.5));
        assert!(a.has("loop"));
        assert_eq!(a.get("ssrc"), Some("1234"));
        assert!(a.get("missing").is_none());
    }

    #[test]
    fn repeated_keys_are_collected() {
        let a = parse(&["--expect-tone", "1000", "--expect-tone", "2000"]);
        assert_eq!(a.get_all("expect-tone"), vec!["1000", "2000"]);
    }

    #[test]
    fn rejects_positional() {
        assert!(Args::parse(vec!["foo".to_string()]).is_err());
    }
}
