//! The harnesses and model providers this build carries, by the `adapter` and `provider` names profiles give them
//! (`docs/factory/seams.md`). The control plane checks config against these lists and the daemon builds its harness
//! from them, so both read the one list here.

use trailhead_factory::{HarnessKind, ProviderKind, Registry};

pub use trailhead_harness_claude_code::{NAME as CLAUDE_CODE, OAUTH_TOKEN as CLAUDE_CODE_OAUTH_TOKEN};
pub use trailhead_provider_anthropic::NAME as ANTHROPIC;

pub fn harnesses() -> Registry<HarnessKind> {
    Registry::new("adapter").with(CLAUDE_CODE, trailhead_harness_claude_code::kind())
}

pub fn providers() -> Registry<ProviderKind> {
    Registry::new("provider").with(ANTHROPIC, trailhead_provider_anthropic::kind())
}

#[cfg(test)]
mod tests {
    use trailhead_factory::{HarnessSetup, Secret};

    use super::*;

    #[test]
    fn claude_code_asks_for_egress_only_in_subscription_mode() {
        let kind = harnesses().get(CLAUDE_CODE).unwrap().clone();
        assert!((kind.egress)(&Default::default()).is_empty());
        let secrets = [(CLAUDE_CODE_OAUTH_TOKEN.to_string(), Secret::new("t"))].into();
        assert_eq!((kind.egress)(&secrets), ["api.anthropic.com"]);
        let _ = (kind.build)(&HarnessSetup { executable: "claude".into(), secrets });
    }

    #[test]
    fn an_unknown_harness_is_refused_by_name() {
        assert!(harnesses().get("nope").is_err());
    }

    #[test]
    fn anthropic_is_a_provider() {
        assert!(providers().get(ANTHROPIC).is_ok());
        assert!(providers().get("nope").is_err());
    }
}
