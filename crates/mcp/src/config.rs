//! Settings from the environment.

use std::fmt;
use std::path::PathBuf;

pub const DEFAULT_API_URL: &str = "http://127.0.0.1:8402";
pub const DEFAULT_SESSION_CAP_USD: &str = "1.00";
pub const DEFAULT_OUTPUT_DIR: &str = "unbaked-output";

/// The network this tool will pay on. Real money is out of scope.
pub const NETWORK: &str = "eip155:84532";

#[derive(Clone)]
pub struct Config {
    /// Base URL of the `unbaked-api` server, with no trailing slash.
    pub api_url: String,
    /// A throwaway test wallet's private key. Without one, paid tools refuse.
    pub wallet_key: Option<WalletKey>,
    /// The most this session will spend in total, in micro-USDC.
    pub session_cap: u64,
    pub output_dir: PathBuf,
}

/// A private key: usable, but never printed.
#[derive(Clone)]
pub struct WalletKey(String);

impl WalletKey {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WalletKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WalletKey(hidden)")
    }
}

impl Config {
    /// Loads `UNBAKED_ENV_FILE` first, if set, then reads settings from the
    /// process environment.
    pub fn from_env() -> Result<Self, String> {
        if let Ok(path) = std::env::var("UNBAKED_ENV_FILE") {
            dotenvy::from_path(&path).map_err(|error| format!("{path}: {error}"))?;
        }
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads settings through `lookup`, so tests need not touch the process
    /// environment. Blank values count as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let api_url = get("UNBAKED_API_URL")
            .unwrap_or_else(|| DEFAULT_API_URL.to_owned())
            .trim_end_matches('/')
            .to_owned();
        let wallet_key = get("UNBAKED_WALLET_KEY").map(WalletKey);
        let cap =
            get("UNBAKED_SESSION_CAP_USD").unwrap_or_else(|| DEFAULT_SESSION_CAP_USD.to_owned());
        let session_cap = micro_dollars(&cap).ok_or_else(|| {
            format!("UNBAKED_SESSION_CAP_USD {cap:?} is not a dollar amount like 1 or 0.50")
        })?;
        let output_dir = get("UNBAKED_OUTPUT_DIR")
            .unwrap_or_else(|| DEFAULT_OUTPUT_DIR.to_owned())
            .into();
        Ok(Self {
            api_url,
            wallet_key,
            session_cap,
            output_dir,
        })
    }
}

/// "2.50" as 2_500_000. At most six decimals. Same rule as the server's daily
/// cap parsing.
pub fn micro_dollars(value: &str) -> Option<u64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(fraction) || fraction.len() > 6 {
        return None;
    }
    let fraction = format!("{fraction:0<6}").parse::<u64>().ok()?;
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(fraction)
}

/// Micro-USDC as a dollar string, e.g. 1_500_000 -> "1.50".
pub fn dollars(micro: u64) -> String {
    format!("{}.{:02}", micro / 1_000_000, (micro % 1_000_000) / 10_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cap_is_read_in_dollars() {
        let load = |value: &str| {
            Config::from_lookup(|name| {
                (name == "UNBAKED_SESSION_CAP_USD").then(|| value.to_owned())
            })
            .map(|c| c.session_cap)
        };
        assert_eq!(load("2.5"), Ok(2_500_000));
        assert_eq!(load("0.000001"), Ok(1));
        assert!(load("-1").is_err());
        assert!(load("$5").is_err());
    }

    #[test]
    fn defaults_are_sensible() {
        let config = Config::from_lookup(|_| None).unwrap();
        assert_eq!(config.api_url, DEFAULT_API_URL);
        assert_eq!(config.session_cap, 1_000_000);
        assert_eq!(config.output_dir, PathBuf::from(DEFAULT_OUTPUT_DIR));
        assert!(config.wallet_key.is_none());
    }

    #[test]
    fn keys_are_never_printed() {
        let config = Config::from_lookup(|name| {
            (name == "UNBAKED_WALLET_KEY").then(|| "0xdo-not-print".to_owned())
        })
        .unwrap();
        assert_eq!(
            config.wallet_key.as_ref().unwrap().expose(),
            "0xdo-not-print"
        );
        assert!(!format!("{:?}", config.wallet_key).contains("do-not-print"));
    }

    #[test]
    fn dollars_format() {
        assert_eq!(dollars(1_000_000), "1.00");
        assert_eq!(dollars(1_500_000), "1.50");
        assert_eq!(dollars(50_000), "0.05");
        assert_eq!(dollars(250_000), "0.25");
    }
}
