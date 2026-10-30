//! `ModelProvider` for Anthropic (`docs/factory/agents.md`): resolves the profile aliases to model ids, names the
//! endpoint sandboxes send model calls to, and prices usage for budgets.

use trailhead_factory::{Endpoint, ModelAlias, ModelId, ModelProvider, Usage, Usd};

/// Where model calls go without a connector.
pub const API_URL: &str = "https://api.anthropic.com";

/// Dollars per million tokens, in micro-dollars, so prices like $0.125 stay exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    /// Five-minute cache writes, about 1.25× input.
    pub cache_write: u64,
}

impl Price {
    pub fn of(&self, usage: &Usage) -> Usd {
        let per_million = u128::from(usage.input_tokens) * u128::from(self.input)
            + u128::from(usage.output_tokens) * u128::from(self.output)
            + u128::from(usage.cache_read_tokens) * u128::from(self.cache_read)
            + u128::from(usage.cache_write_tokens) * u128::from(self.cache_write);
        // Rounded up: budgets should err towards stopping.
        Usd(u64::try_from(per_million.div_ceil(1_000_000)).unwrap_or(u64::MAX))
    }
}

/// First-party API prices as of 2026-10-06 (`claude-api` skill, which says to confirm against Anthropic's pricing
/// page). Haiku's rate is for prompts up to 100K tokens; longer ones cost five times as much and are underpriced here.
pub const OPUS: Price = Price { input: 4_000_000, output: 20_000_000, cache_read: 200_000, cache_write: 5_000_000 };
pub const SONNET: Price = Price { input: 2_000_000, output: 10_000_000, cache_read: 200_000, cache_write: 2_500_000 };
pub const HAIKU: Price = Price { input: 100_000, output: 500_000, cache_read: 10_000, cache_write: 125_000 };

pub struct Anthropic {
    endpoint: Endpoint,
}

impl Anthropic {
    /// `base_url` is a connector's gateway URL, or [`API_URL`].
    pub fn new(base_url: impl Into<String>) -> Anthropic {
        Anthropic { endpoint: Endpoint { base_url: base_url.into() } }
    }

    /// Checks an API key with the cheapest authenticated call, `GET /v1/models`, against `base_url`.
    pub async fn check_key(base_url: &str, key: &str) -> Result<(), String> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("trailhead-factory")
            .build()
            .map_err(|e| e.to_string())?;
        let res = http
            .get(format!("{}/v1/models", base_url.trim_end_matches('/')))
            .query(&[("limit", "1")])
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .map_err(|e| format!("calling {base_url}: {e}"))?;
        match res.status() {
            s if s.is_success() => Ok(()),
            s => Err(format!("{s} from GET /v1/models")),
        }
    }

    /// The price for a model id as Claude Code reports it. An id this table doesn't know is priced as Opus, the most
    /// expensive, so a budget stops early rather than late.
    pub fn price_for(model: &ModelId) -> Price {
        let id = model.0.as_str();
        if id.starts_with("claude-haiku") {
            HAIKU
        } else if id.starts_with("claude-sonnet") {
            SONNET
        } else {
            if !id.starts_with("claude-opus") {
                tracing::warn!(model = id, "no price for this model; pricing it as Opus");
            }
            OPUS
        }
    }
}

impl ModelProvider for Anthropic {
    fn resolve(&self, alias: ModelAlias) -> ModelId {
        ModelId(
            match alias {
                ModelAlias::Opus => "claude-opus-5-5",
                ModelAlias::Sonnet => "claude-sonnet-5-5",
                ModelAlias::Haiku => "claude-haiku-5-5",
            }
            .into(),
        )
    }

    fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    fn price(&self, model: &ModelId, usage: &Usage) -> Usd {
        Anthropic::price_for(model).of(usage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
        }
    }

    #[test]
    fn prices_per_million_tokens() {
        let a = Anthropic::new(API_URL);
        let sonnet = a.resolve(ModelAlias::Sonnet);
        assert_eq!(sonnet.0, "claude-sonnet-5-5");
        assert_eq!(a.price(&sonnet, &usage(1_000_000, 0, 0, 0)), Usd::dollars(2.0));
        assert_eq!(a.price(&sonnet, &usage(0, 1_000_000, 0, 0)), Usd::dollars(10.0));
        let opus = a.resolve(ModelAlias::Opus);
        assert_eq!(a.price(&opus, &usage(10_000, 2_000, 50_000, 4_000)), Usd::dollars(0.04 + 0.04 + 0.01 + 0.02));
        let haiku = a.resolve(ModelAlias::Haiku);
        assert_eq!(
            a.price(&haiku, &usage(1_000_000, 1_000_000, 1_000_000, 1_000_000)),
            Usd::dollars(0.1 + 0.5 + 0.01 + 0.125)
        );
    }

    #[test]
    fn rounds_up_and_prices_unknown_models_as_opus() {
        let a = Anthropic::new(API_URL);
        assert_eq!(
            a.price(&ModelId("claude-haiku-5-5".into()), &usage(1, 0, 0, 0)),
            Usd(1),
            "a tenth of a micro rounds up"
        );
        let unknown = ModelId("claude-something-new".into());
        assert_eq!(a.price(&unknown, &usage(1_000_000, 0, 0, 0)), Usd::dollars(4.0));
    }
}
