use dotenvy::dotenv;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub fork_url: Option<String>,
    pub etherscan_key: Option<String>,
    pub api_key: Option<String>,
    pub whitelist_chain_ids: Vec<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8080,
            fork_url: None,
            etherscan_key: None,
            api_key: None,
            whitelist_chain_ids: DEFAULT_WHITELIST_CHAIN_IDS.to_vec(),
        }
    }
}

pub fn config() -> Config {
    dotenv().ok();

    load_config()
}

pub const DEFAULT_WHITELIST_CHAIN_IDS: &[u64] = &[1, 137, 56, 8217, 97];

/// Parses a comma-separated whitelist. An explicitly empty value disables all chains.
fn parse_whitelist(value: &str) -> Result<Vec<u64>, &'static str> {
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    value.split(',').map(|item| {
        let item = item.trim();
        if item.is_empty() || !item.bytes().all(|b| b.is_ascii_digit()) {
            return Err("WHITELIST_CHAIN_IDS must contain positive integer chain IDs separated by commas");
        }
        item.parse::<u64>().ok().filter(|id| *id > 0)
            .ok_or("WHITELIST_CHAIN_IDS must contain positive integer chain IDs separated by commas")
    }).collect()
}

impl Config {
    /// Rejects unsupported chains before simulation can issue RPC requests.
    pub fn ensure_chain_supported(&self, chain_id: u64) -> Result<(), warp::Rejection> {
        if self.whitelist_chain_ids.contains(&chain_id) {
            Ok(())
        } else {
            Err(warp::reject::custom(crate::errors::NoURLForChainIdError))
        }
    }
}

fn load_config() -> Config {
    let port = std::env::var("PORT")
        .unwrap_or("8080".to_string())
        .parse::<u16>()
        .expect("PORT must be a valid u16.");
    let fork_url = std::env::var("FORK_URL").ok().filter(|k| !k.is_empty());
    let etherscan_key = std::env::var("ETHERSCAN_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    let api_key = std::env::var("API_KEY").ok().filter(|k| !k.is_empty());

    let whitelist_chain_ids = match std::env::var("WHITELIST_CHAIN_IDS") {
        Ok(value) => parse_whitelist(&value).expect("Invalid WHITELIST_CHAIN_IDS"),
        Err(std::env::VarError::NotPresent) => DEFAULT_WHITELIST_CHAIN_IDS.to_vec(),
        Err(_) => panic!("WHITELIST_CHAIN_IDS must be valid Unicode"),
    };

    Config {
        whitelist_chain_ids,
        fork_url,
        port,
        etherscan_key,
        api_key,
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn whitelist_defaults_and_explicit_values() {
        temp_env::with_var("WHITELIST_CHAIN_IDS", None::<&str>, || {
            let config = super::load_config();
            assert_eq!(config.whitelist_chain_ids, [1, 137, 56, 8217, 97]);
            for id in [1, 137, 56, 8217, 97] {
                assert!(config.ensure_chain_supported(id).is_ok());
            }
            assert!(config.ensure_chain_supported(10).is_err());
            assert!(config.ensure_chain_supported(0).is_err());
        });
        temp_env::with_var("WHITELIST_CHAIN_IDS", Some(" 1, 56 "), || {
            let config = super::load_config();
            assert_eq!(config.whitelist_chain_ids, [1, 56]);
            assert!(config.ensure_chain_supported(137).is_err());
        });
        temp_env::with_var("WHITELIST_CHAIN_IDS", Some(""), || {
            let config = super::load_config();
            assert!(config.whitelist_chain_ids.is_empty());
            assert!(config.ensure_chain_supported(1).is_err());
        });
    }

    #[test]
    fn whitelist_rejects_malformed_values() {
        for value in [
            "abc",
            "0",
            "-1",
            "+1",
            "1,",
            ",1",
            "1,,56",
            "18446744073709551616",
        ] {
            assert!(super::parse_whitelist(value).is_err(), "{value}");
        }
    }

    #[test]
    #[should_panic(expected = "Invalid WHITELIST_CHAIN_IDS")]
    fn invalid_whitelist_fails_at_startup() {
        temp_env::with_var("WHITELIST_CHAIN_IDS", Some("1,nope"), || {
            super::load_config();
        });
    }
    #[test]
    #[should_panic(expected = "PORT must be a valid u16.")]
    fn test_config_port_number() {
        temp_env::with_var("PORT", Some("not a number"), || {
            super::load_config();
        });
    }

    #[test]
    fn test_config_fork_url() {
        temp_env::with_vars([("FORK_URL", Some("a"))], || {
            let config = super::load_config();
            assert_eq!(config.fork_url, Some("a".to_string()));
        });

        temp_env::with_vars([("FORK_URL", Some(""))], || {
            let config = super::load_config();
            assert_eq!(config.fork_url, None);
        });

        temp_env::with_vars_unset([("FORK_URL")], || {
            let config = super::load_config();
            assert_eq!(config.fork_url, None);
        });
    }

    #[test]
    fn test_config_etherscan_key() {
        temp_env::with_vars([("ETHERSCAN_KEY", Some("a"))], || {
            let config = super::load_config();
            assert_eq!(config.etherscan_key, Some("a".to_string()));
        });

        temp_env::with_vars([("ETHERSCAN_KEY", Some(""))], || {
            let config = super::load_config();
            assert_eq!(config.etherscan_key, None);
        });

        temp_env::with_vars_unset([("ETHERSCAN_KEY")], || {
            let config = super::load_config();
            assert_eq!(config.etherscan_key, None);
        });
    }

    #[test]
    fn test_config_api_key() {
        temp_env::with_vars([("API_KEY", Some("a"))], || {
            let config = super::load_config();
            assert_eq!(config.api_key, Some("a".to_string()));
        });

        temp_env::with_vars([("API_KEY", Some(""))], || {
            let config = super::load_config();
            assert_eq!(config.api_key, None);
        });

        temp_env::with_vars_unset([("API_KEY")], || {
            let config = super::load_config();
            assert_eq!(config.api_key, None);
        });
    }
}
