//! The records a connector hands over, and the check of a set against the feed's declared fields.

use serde_json::{Map, Value};

use crate::manifest::{FeedSpec, Manifest};

#[derive(Debug, Clone, PartialEq)]
pub struct FeedRecord {
    pub key: String,
    pub data: Map<String, Value>,
}

impl FeedRecord {
    pub fn new(key: impl Into<String>, data: Map<String, Value>) -> FeedRecord {
        FeedRecord { key: key.into(), data }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FeedError {
    #[error("the connector declares no feed `{0}`")]
    UnknownFeed(String),
    #[error("feed `{feed}`: a record has an empty key")]
    EmptyKey { feed: String },
    #[error("feed `{feed}`: key `{key}` appears more than once")]
    DuplicateKey { feed: String, key: String },
    #[error("feed `{feed}`, record `{key}`: missing field `{field}`")]
    Missing { feed: String, key: String, field: String },
    #[error("feed `{feed}`, record `{key}`: `{field}` is not a declared field")]
    Unknown { feed: String, key: String, field: String },
    #[error("feed `{feed}`, record `{key}`, field `{field}`: {problem}")]
    Mistyped { feed: String, key: String, field: String, problem: String },
}

impl FeedSpec {
    pub fn check(&self, feed: &str, records: &[FeedRecord]) -> Result<(), FeedError> {
        let mut seen = std::collections::BTreeSet::new();
        for record in records {
            let at = |field: &str| (feed.to_string(), record.key.clone(), field.to_string());
            if record.key.is_empty() {
                return Err(FeedError::EmptyKey { feed: feed.to_string() });
            }
            if !seen.insert(record.key.as_str()) {
                return Err(FeedError::DuplicateKey { feed: feed.to_string(), key: record.key.clone() });
            }
            if let Some(field) = record.data.keys().find(|f| !self.fields.contains_key(*f)) {
                let (feed, key, field) = at(field);
                return Err(FeedError::Unknown { feed, key, field });
            }
            for (field, ty) in &self.fields {
                match record.data.get(field) {
                    None if ty.optional => {}
                    None => {
                        let (feed, key, field) = at(field);
                        return Err(FeedError::Missing { feed, key, field });
                    }
                    Some(value) => {
                        if let Err(problem) = ty.check(value).map_err(|e| e.0) {
                            let (feed, key, field) = at(field);
                            return Err(FeedError::Mistyped { feed, key, field, problem });
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Manifest {
    pub fn check_feed(&self, feed: &str, records: &[FeedRecord]) -> Result<(), FeedError> {
        self.feeds.get(feed).ok_or_else(|| FeedError::UnknownFeed(feed.to_string()))?.check(feed, records)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::manifest::tests::GITHUB;

    fn record(key: &str, data: Value) -> FeedRecord {
        FeedRecord::new(key, data.as_object().unwrap().clone())
    }

    fn good() -> Value {
        json!({"title": "t", "url": "https://github.com/a/b/pull/1", "author": "lane", "draft": false,
               "updated": "2026-11-22T10:00:00Z", "reviewers": ["x"]})
    }

    fn check(records: &[FeedRecord]) -> Result<(), FeedError> {
        Manifest::parse(GITHUB).unwrap().check_feed("pull_requests", records)
    }

    #[test]
    fn accepts_a_good_set_with_optional_fields_absent_or_null() {
        let mut with_null = good();
        with_null["ticket"] = Value::Null;
        assert_eq!(check(&[record("1", good()), record("2", with_null)]), Ok(()));
        assert_eq!(check(&[]), Ok(()));
    }

    #[test]
    fn names_the_feed_key_and_field() {
        let mut missing = good();
        missing.as_object_mut().unwrap().remove("title");
        let e = check(&[record("7", missing)]).unwrap_err();
        assert_eq!(e.to_string(), "feed `pull_requests`, record `7`: missing field `title`");

        let mut extra = good();
        extra["stars"] = json!(1);
        let e = check(&[record("7", extra)]).unwrap_err();
        assert_eq!(e.to_string(), "feed `pull_requests`, record `7`: `stars` is not a declared field");

        let mut wrong = good();
        wrong["draft"] = json!("no");
        let e = check(&[record("7", wrong)]).unwrap_err().to_string();
        assert!(e.starts_with("feed `pull_requests`, record `7`, field `draft`: should be true or false"), "{e}");
    }

    #[test]
    fn refuses_empty_and_duplicate_keys_and_unknown_feeds() {
        assert_eq!(check(&[record("", good())]), Err(FeedError::EmptyKey { feed: "pull_requests".into() }));
        assert_eq!(
            check(&[record("1", good()), record("1", good())]),
            Err(FeedError::DuplicateKey { feed: "pull_requests".into(), key: "1".into() })
        );
        let m = Manifest::parse(GITHUB).unwrap();
        assert_eq!(m.check_feed("issues", &[]), Err(FeedError::UnknownFeed("issues".into())));
    }
}
