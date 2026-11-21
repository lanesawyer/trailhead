//! Pictures attached to a task: what one is, and the limits on them, which the engine enforces and a client can check
//! before uploading.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ParseError;

/// The only content type accepted for now.
pub const PNG: &str = "image/png";
/// The largest picture, in bytes.
pub const MAX_ATTACHMENT_BYTES: usize = 2 * 1024 * 1024;
/// The most live attachments one task holds.
pub const MAX_ATTACHMENTS_PER_TASK: usize = 64;
/// The most bytes one task's live attachments hold together, counting each attachment whether or not its bytes are
/// stored once for several.
pub const MAX_TASK_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;
/// The longest name, in characters.
pub const MAX_ATTACHMENT_NAME: usize = 64;
/// The longest set, in characters.
pub const MAX_ATTACHMENT_SET: usize = 64;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Which side of a change a picture shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRole {
    Before,
    After,
}

impl AttachmentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            AttachmentRole::Before => "before",
            AttachmentRole::After => "after",
        }
    }
}

impl std::fmt::Display for AttachmentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AttachmentRole {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "before" => Ok(AttachmentRole::Before),
            "after" => Ok(AttachmentRole::After),
            _ => Err(ParseError(format!("unknown attachment role: {s} (expected one of: before, after)"))),
        }
    }
}

/// A picture on a task. This is its metadata alone: the bytes are read by id, and are stored once for every
/// attachment with the same `sha256`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Attachment {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub task_id: Uuid,
    /// A short label, such as `ticklist-today-dark`: what a before and an after of the same view share.
    pub name: String,
    pub content_type: String,
    /// Bytes.
    pub size: i64,
    /// Lowercase hex SHA-256 of the bytes, which is also their `ETag`.
    pub sha256: String,
    /// Pixels, from the PNG header.
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<AttachmentRole>,
    /// Groups pictures taken together, such as a run id or a commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    pub created_by: Uuid,
    pub on_behalf_of: Uuid,
    pub created_at: Timestamp,
    pub deleted_at: Option<Timestamp>,
}

/// The width and height of a PNG, read from its signature and header chunk. Anything else is refused, whatever its
/// content type claims.
pub fn png_size(bytes: &[u8]) -> Result<(u32, u32), ParseError> {
    let not_png = || ParseError("the picture isn't a PNG".into());
    if bytes.len() < 33 || bytes[..8] != PNG_SIGNATURE || bytes[8..12] != [0, 0, 0, 13] || &bytes[12..16] != b"IHDR" {
        return Err(not_png());
    }
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
    let (width, height) = (word(16), word(20));
    if width == 0 || height == 0 {
        return Err(not_png());
    }
    Ok((width, height))
}

/// An attachment's name, trimmed: 1 to [`MAX_ATTACHMENT_NAME`] characters of ASCII letters, digits, `.`, `_` and `-`,
/// starting with a letter or digit.
pub fn attachment_name(name: &str) -> Result<String, ParseError> {
    let name = name.trim();
    let ok = (1..=MAX_ATTACHMENT_NAME).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ok {
        return Err(ParseError(format!(
            "an attachment name is 1 to {MAX_ATTACHMENT_NAME} letters, digits, '.', '_' or '-', starting with a \
             letter or digit: {name:?}"
        )));
    }
    Ok(name.to_string())
}

/// An attachment's set, trimmed: 1 to [`MAX_ATTACHMENT_SET`] characters with no control characters.
pub fn attachment_set(set: &str) -> Result<String, ParseError> {
    let set = set.trim();
    let len = set.chars().count();
    if len == 0 || len > MAX_ATTACHMENT_SET || set.chars().any(char::is_control) {
        return Err(ParseError(format!(
            "an attachment set is 1 to {MAX_ATTACHMENT_SET} characters with no control characters"
        )));
    }
    Ok(set.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: u32, height: u32) -> Vec<u8> {
        let mut b = PNG_SIGNATURE.to_vec();
        b.extend([0, 0, 0, 13]);
        b.extend(b"IHDR");
        b.extend(width.to_be_bytes());
        b.extend(height.to_be_bytes());
        b.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b
    }

    #[test]
    fn a_png_header_gives_its_size_and_anything_else_is_refused() {
        assert_eq!(png_size(&header(640, 480)), Ok((640, 480)));
        assert!(png_size(&header(0, 480)).is_err());
        assert!(png_size(b"GIF89a and then some more bytes to pass the length").is_err());
        assert!(png_size(&header(1, 1)[..20]).is_err());
    }

    #[test]
    fn names_and_sets_are_bounded() {
        assert_eq!(attachment_name(" ticklist-today-dark ").unwrap(), "ticklist-today-dark");
        for bad in ["", "-dash", "a b", "a/b", "é", &"x".repeat(MAX_ATTACHMENT_NAME + 1)] {
            assert!(attachment_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(attachment_set(" run 0193 ").unwrap(), "run 0193");
        for bad in ["", "  ", "a\nb", &"x".repeat(MAX_ATTACHMENT_SET + 1)] {
            assert!(attachment_set(bad).is_err(), "{bad:?}");
        }
    }
}
