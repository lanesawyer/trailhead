use trailhead_factory::{Endpoint, ModelAlias, ModelId, ModelProvider, Usage, Usd};

/// Prices every token the same, so tests can reason about spend in round numbers.
pub struct FakeProvider {
    pub micros_per_token: u64,
}

impl FakeProvider {
    pub fn new(micros_per_token: u64) -> FakeProvider {
        FakeProvider { micros_per_token }
    }
}

impl ModelProvider for FakeProvider {
    fn resolve(&self, alias: ModelAlias) -> ModelId {
        let name = match alias {
            ModelAlias::Opus => "opus",
            ModelAlias::Sonnet => "sonnet",
            ModelAlias::Haiku => "haiku",
        };
        ModelId(format!("fake-{name}"))
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint { base_url: "http://gateway.invalid/anthropic".into() }
    }

    fn price(&self, _model: &ModelId, usage: &Usage) -> Usd {
        let tokens = usage.input_tokens + usage.output_tokens + usage.cache_read_tokens + usage.cache_write_tokens;
        Usd(tokens * self.micros_per_token)
    }
}
