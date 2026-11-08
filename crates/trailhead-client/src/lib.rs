//! What a client of the Trailhead API shares, with no server, database or UI
//! dependency: the CLI and Lookout both build on it.
//!
//! - [`creds`]: where saved tokens live (`<config dir>/servers/<instance>/` and the keyring entry).
//! - [`Health`] and [`ApiError`]: the bodies a client reads from `/v1/health` and from failed requests.
//! - [`paths`]: where an instance keeps its data and config ([`paths::Paths`]), and the local server's default address.
//! - [`text`]: how the CLI and MCP show tasks, projects, comments, claims and events.

pub mod creds;
pub mod echo;
pub mod paths;
pub mod text;

use std::path::PathBuf;

/// `/v1/health` as a client reads it. Every field is optional so an older or newer
/// server still counts as running.
#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Health {
    pub version: Option<String>,
    pub instance: Option<String>,
    pub pid: Option<u32>,
    pub exe: Option<PathBuf>,
    pub exe_modified_ms: Option<u64>,
    pub db: Option<PathBuf>,
    pub started_at: Option<String>,
    /// The factory's runs in flight, from a server that runs its control plane.
    pub runs_in_flight: Option<usize>,
    /// Whether a stop signal has the server draining the factory before it exits.
    pub draining: bool,
    /// Whether the server restarts onto a newer binary once the factory's runs finish.
    pub upgrade_pending: bool,
}

/// An API error with its stable code, so a client can print `error[claim_held]: ...`.
#[derive(Debug)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    /// Reads the `{"error": {"code", "message"}}` body of a failed response.
    pub fn from_body(body: &serde_json::Value) -> Self {
        let err = &body["error"];
        ApiError {
            code: err["code"].as_str().unwrap_or("unknown").into(),
            message: err["message"].as_str().unwrap_or("request failed").into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error[{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_reads_the_error_body() {
        let e = ApiError::from_body(&serde_json::json!({"error": {"code": "claim_held", "message": "held"}}));
        assert_eq!(e.to_string(), "error[claim_held]: held");
        assert_eq!(ApiError::from_body(&serde_json::json!({})).to_string(), "error[unknown]: request failed");
    }

    #[test]
    fn health_tolerates_missing_fields() {
        let h: Health = serde_json::from_str(r#"{"ok":true,"version":"1"}"#).unwrap();
        assert_eq!(h.version.as_deref(), Some("1"));
        assert!(h.pid.is_none());
    }
}
