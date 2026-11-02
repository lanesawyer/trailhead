//! The harnesses this build carries, by the `adapter` name profiles give them. The control plane checks config
//! against it and the daemon builds from it.

use std::sync::Arc;

use trailhead_factory::{HarnessKind, Registry};
use trailhead_harness_claude_code::ClaudeCode;

pub use trailhead_harness_claude_code::{NAME as CLAUDE_CODE, OAUTH_TOKEN as CLAUDE_CODE_OAUTH_TOKEN};

pub fn harnesses() -> Registry<HarnessKind> {
    Registry::new("adapter").with(
        CLAUDE_CODE,
        HarnessKind {
            build: |setup| {
                let claude = ClaudeCode::new(setup.executable.clone());
                Arc::new(match setup.secrets.get(CLAUDE_CODE_OAUTH_TOKEN) {
                    Some(token) => claude.with_oauth_token(token.clone()),
                    None => claude,
                })
            },
            egress: |secrets| {
                if secrets.contains_key(CLAUDE_CODE_OAUTH_TOKEN) {
                    vec![trailhead_harness_claude_code::API_HOST.into()]
                } else {
                    Vec::new()
                }
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use trailhead_factory::HarnessSetup;

    #[test]
    fn claude_code_asks_for_egress_only_in_subscription_mode() {
        let kind = *harnesses().get(CLAUDE_CODE).unwrap();
        assert!((kind.egress)(&Default::default()).is_empty());
        let secrets = [(CLAUDE_CODE_OAUTH_TOKEN.to_string(), "t".to_string())].into();
        assert_eq!((kind.egress)(&secrets), ["api.anthropic.com"]);
        let _ = (kind.build)(&HarnessSetup { executable: "claude".into(), secrets });
    }

    #[test]
    fn an_unknown_harness_is_refused_by_name() {
        assert!(harnesses().get("nope").is_err());
    }
}
