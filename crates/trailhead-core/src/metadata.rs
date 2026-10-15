use serde_json::Value;

use crate::error::{Error, Result};

/// RFC 7386 JSON merge patch. Metadata must stay an object.
pub(crate) fn merge_patch(target: &Value, patch: &Value) -> Result<Value> {
    if !patch.is_object() {
        return Err(Error::Invalid("metadata patch must be a JSON object".into()));
    }
    Ok(merge(target.clone(), patch))
}

fn merge(target: Value, patch: &Value) -> Value {
    let Value::Object(p) = patch else {
        return patch.clone();
    };
    let mut t = match target {
        Value::Object(t) => t,
        _ => Default::default(),
    };
    for (k, v) in p {
        if v.is_null() {
            t.remove(k);
        } else {
            let existing = t.remove(k).unwrap_or(Value::Null);
            t.insert(k.clone(), merge(existing, v));
        }
    }
    Value::Object(t)
}

/// Metadata filter paths are interpolated into a JSON path, so keep them boring.
pub(crate) fn validate_path(path: &str) -> Result<()> {
    let ok = !path.is_empty()
        && path
            .split('.')
            .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    if ok { Ok(()) } else { Err(Error::Invalid(format!("bad metadata key {path:?}"))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_adds_replaces_and_removes() {
        let t = json!({"a": 1, "b": {"c": 2, "d": 3}, "e": 4});
        let p = json!({"a": 9, "b": {"c": null}, "e": null, "f": [1]});
        assert_eq!(merge_patch(&t, &p).unwrap(), json!({"a": 9, "b": {"d": 3}, "f": [1]}));
    }

    #[test]
    fn rejects_injection_in_paths() {
        assert!(validate_path("team.owner").is_ok());
        assert!(validate_path("a') OR 1=1 --").is_err());
        assert!(validate_path("a..b").is_err());
    }
}
