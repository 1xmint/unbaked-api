//! Settings from the environment. The test network is the default; real money
//! needs two settings, both on purpose.

use std::fmt;
use std::net::SocketAddr;

pub const DEFAULT_ADDR: &str = "127.0.0.1:8402";

/// The free public facilitator, which needs no key. Test network only.
pub const TEST_FACILITATOR: &str = "https://x402.org/facilitator";

/// The most provider cost allowed per UTC day unless set: $5.
pub const DEFAULT_DAILY_CAP: u64 = 5_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    BaseSepolia,
    Base,
}

impl Network {
    /// The CAIP-2 id x402 uses for the network.
    pub fn caip2(self) -> &'static str {
        match self {
            Self::BaseSepolia => "eip155:84532",
            Self::Base => "eip155:8453",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub addr: SocketAddr,
    pub network: Network,
    /// Base URL of the facilitator, with no trailing slash.
    pub facilitator: String,
    /// Where payments go. Paid routes refuse to run without it.
    pub pay_to: Option<String>,
    /// The most provider cost allowed per UTC day, in millionths of a dollar.
    pub daily_cap: u64,
    /// OpenAI's key. Picture routes refuse without it.
    pub openai_key: Option<Secret>,
}

/// A key: usable, but never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(hidden)")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    UnknownNetwork(String),
    RealMoneyNotAllowed,
    Missing(&'static str),
    BadAddr(String),
    BadFacilitator(String),
    BadPayTo(String),
    BadDailyCap(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNetwork(name) => write!(
                f,
                "UNBAKED_API_NETWORK is {name:?}; use \"base-sepolia\" (the default) or \"base\""
            ),
            Self::RealMoneyNotAllowed => write!(
                f,
                "UNBAKED_API_NETWORK=base moves real money; it also needs UNBAKED_API_REAL_MONEY=yes"
            ),
            Self::Missing(name) => write!(f, "{name} is required on the real-money network"),
            Self::BadAddr(value) => write!(f, "UNBAKED_API_ADDR {value:?} is not an address:port"),
            Self::BadFacilitator(value) => write!(
                f,
                "UNBAKED_API_FACILITATOR {value:?} must be an https:// URL (http:// only on the test network)"
            ),
            Self::BadPayTo(value) => write!(
                f,
                "UNBAKED_API_PAY_TO {value:?} is not an EVM address (0x and 40 hex digits)"
            ),
            Self::BadDailyCap(value) => write!(
                f,
                "UNBAKED_API_DAILY_CAP_USD {value:?} is not a dollar amount like 5 or 2.50"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads settings through `lookup`, so tests need not touch the process
    /// environment. Blank values count as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };

        let network = match get("UNBAKED_API_NETWORK").as_deref() {
            None | Some("base-sepolia") => Network::BaseSepolia,
            Some("base") => {
                if get("UNBAKED_API_REAL_MONEY").as_deref() != Some("yes") {
                    return Err(ConfigError::RealMoneyNotAllowed);
                }
                Network::Base
            }
            Some(other) => return Err(ConfigError::UnknownNetwork(other.to_owned())),
        };

        let addr = get("UNBAKED_API_ADDR").unwrap_or_else(|| DEFAULT_ADDR.to_owned());
        let addr = addr.parse().map_err(|_| ConfigError::BadAddr(addr))?;

        let facilitator = match (get("UNBAKED_API_FACILITATOR"), network) {
            (Some(url), _) => url,
            (None, Network::BaseSepolia) => TEST_FACILITATOR.to_owned(),
            (None, Network::Base) => return Err(ConfigError::Missing("UNBAKED_API_FACILITATOR")),
        };
        let secure = facilitator.starts_with("https://");
        let local_test = network == Network::BaseSepolia && facilitator.starts_with("http://");
        if !(secure || local_test) {
            return Err(ConfigError::BadFacilitator(facilitator));
        }
        let facilitator = facilitator.trim_end_matches('/').to_owned();

        let pay_to = get("UNBAKED_API_PAY_TO");
        match (&pay_to, network) {
            (Some(address), _) if !is_evm_address(address) => {
                return Err(ConfigError::BadPayTo(address.clone()));
            }
            (None, Network::Base) => return Err(ConfigError::Missing("UNBAKED_API_PAY_TO")),
            _ => {}
        }

        let daily_cap = match get("UNBAKED_API_DAILY_CAP_USD") {
            None => DEFAULT_DAILY_CAP,
            Some(value) => micro_dollars(&value).ok_or(ConfigError::BadDailyCap(value))?,
        };

        Ok(Self {
            addr,
            network,
            facilitator,
            pay_to,
            daily_cap,
            openai_key: get("OPENAI_API_KEY").map(Secret::new),
        })
    }
}

/// "2.50" as 2_500_000. At most six decimals.
fn micro_dollars(value: &str) -> Option<u64> {
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

fn is_evm_address(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|hex| hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_never_printed() {
        let config = Config::from_lookup(|name| {
            (name == "OPENAI_API_KEY").then(|| "sk-do-not-print".to_owned())
        })
        .unwrap();
        assert_eq!(
            config.openai_key.as_ref().unwrap().expose(),
            "sk-do-not-print"
        );
        assert!(!format!("{config:?}").contains("do-not-print"));
    }

    const PAY_TO: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";

    fn load(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        Config::from_lookup(|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn the_daily_cap_is_read_in_dollars() {
        assert_eq!(load(&[]).unwrap().daily_cap, DEFAULT_DAILY_CAP);
        let cap = |value| load(&[("UNBAKED_API_DAILY_CAP_USD", value)]).map(|c| c.daily_cap);
        assert_eq!(cap("2.5"), Ok(2_500_000));
        assert_eq!(cap("0.000001"), Ok(1));
        assert_eq!(cap("12"), Ok(12_000_000));
        for bad in ["-1", "1.0000001", "$5", ".5", "5e3"] {
            assert_eq!(cap(bad), Err(ConfigError::BadDailyCap(bad.to_owned())));
        }
    }

    #[test]
    fn defaults_to_the_test_network_and_free_facilitator() {
        let config = load(&[]).unwrap();
        assert_eq!(config.network, Network::BaseSepolia);
        assert_eq!(config.network.caip2(), "eip155:84532");
        assert_eq!(config.facilitator, TEST_FACILITATOR);
        assert_eq!(config.addr.to_string(), DEFAULT_ADDR);
        assert_eq!(config.pay_to, None);
    }

    #[test]
    fn real_money_needs_the_second_setting() {
        assert_eq!(
            load(&[("UNBAKED_API_NETWORK", "base")]).unwrap_err(),
            ConfigError::RealMoneyNotAllowed
        );
        assert_eq!(
            load(&[
                ("UNBAKED_API_NETWORK", "base"),
                ("UNBAKED_API_REAL_MONEY", "true")
            ])
            .unwrap_err(),
            ConfigError::RealMoneyNotAllowed
        );
    }

    #[test]
    fn real_money_needs_its_own_facilitator_and_address() {
        let base = [
            ("UNBAKED_API_NETWORK", "base"),
            ("UNBAKED_API_REAL_MONEY", "yes"),
        ];
        assert_eq!(
            load(&base).unwrap_err(),
            ConfigError::Missing("UNBAKED_API_FACILITATOR")
        );

        let with_facilitator = [
            base[0],
            base[1],
            ("UNBAKED_API_FACILITATOR", "https://f.example"),
        ];
        assert_eq!(
            load(&with_facilitator).unwrap_err(),
            ConfigError::Missing("UNBAKED_API_PAY_TO")
        );

        let plain_http = [
            base[0],
            base[1],
            ("UNBAKED_API_FACILITATOR", "http://f.example"),
        ];
        assert!(matches!(
            load(&plain_http).unwrap_err(),
            ConfigError::BadFacilitator(_)
        ));

        let full = [
            base[0],
            base[1],
            ("UNBAKED_API_FACILITATOR", "https://f.example/"),
            ("UNBAKED_API_PAY_TO", PAY_TO),
        ];
        let config = load(&full).unwrap();
        assert_eq!(config.network, Network::Base);
        assert_eq!(config.facilitator, "https://f.example");
    }

    #[test]
    fn refuses_bad_values() {
        assert_eq!(
            load(&[("UNBAKED_API_NETWORK", "solana")]).unwrap_err(),
            ConfigError::UnknownNetwork("solana".into())
        );
        assert!(matches!(
            load(&[("UNBAKED_API_PAY_TO", "0x1234")]).unwrap_err(),
            ConfigError::BadPayTo(_)
        ));
        assert!(matches!(
            load(&[("UNBAKED_API_ADDR", "localhost")]).unwrap_err(),
            ConfigError::BadAddr(_)
        ));
        assert!(matches!(
            load(&[("UNBAKED_API_FACILITATOR", "ftp://f.example")]).unwrap_err(),
            ConfigError::BadFacilitator(_)
        ));
    }

    #[test]
    fn blank_values_count_as_unset() {
        let config = load(&[("UNBAKED_API_PAY_TO", "  "), ("UNBAKED_API_NETWORK", "")]).unwrap();
        assert_eq!(config.network, Network::BaseSepolia);
        assert_eq!(config.pay_to, None);
    }

    #[test]
    fn a_local_http_facilitator_is_allowed_on_the_test_network() {
        let config = load(&[
            ("UNBAKED_API_FACILITATOR", "http://127.0.0.1:9000"),
            ("UNBAKED_API_PAY_TO", PAY_TO),
        ])
        .unwrap();
        assert_eq!(config.facilitator, "http://127.0.0.1:9000");
        assert_eq!(config.pay_to.as_deref(), Some(PAY_TO));
    }
}
