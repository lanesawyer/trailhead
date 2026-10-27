use serde::{Deserialize, Serialize};

use crate::{Usage, Usd};

/// A provider-neutral model name a profile asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelAlias {
    Opus,
    Sonnet,
    Haiku,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId(pub String);

/// Where a sandbox sends model calls, such as a connector gateway's base URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub base_url: String,
}

/// The provider usage API (`spend`) is out of the MVP and joins this trait
/// with the daily cap.
pub trait ModelProvider: Send + Sync {
    fn resolve(&self, alias: ModelAlias) -> ModelId;
    fn endpoint(&self) -> Endpoint;
    fn price(&self, model: &ModelId, usage: &Usage) -> Usd;
}
