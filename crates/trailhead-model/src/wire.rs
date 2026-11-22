//! The bodies of the task API's writes, declared once for both ends: the server reads them and the client sends
//! them, so a field one end gains the other has too. Days and times are the strings the API takes (`YYYY-MM-DD`,
//! RFC 3339 with an offset); the server parses them into the store's types.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{Grade, Priority, Status};

/// Reads a field that may be left out or sent as `null`: absent is `None`, an explicit `null` is `Some(None)`, and a
/// value is `Some(Some(value))`. With `#[serde(default)]` on the field.
pub fn nullable<'de, T: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// `POST /v1/tasks`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateTask {
    pub project: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// 1 (urgent) to 4 (low). Defaults to 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// The day it's due. Optional when `due_at` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = Date))]
    pub due_on: Option<String>,
    /// A time it's due by. Without one, it's due by the end of `due_on` in `due_tz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub due_at: Option<String>,
    /// IANA zone the deadline was set in; decides its day and when that day ends. Without one, UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_tz: Option<String>,
    /// The day to work the task (`YYYY-MM-DD`), separate from its due date.
    /// Optional when `scheduled_at` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = Date))]
    pub scheduled_on: Option<String>,
    /// A time on the scheduled day, e.g. 1pm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub scheduled_at: Option<String>,
    /// IANA zone `scheduled_at` was planned in; decides its day. Without one, the UTC day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_tz: Option<String>,
    /// The key of the task this one goes under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// 1 to 4. Only a human can create a task with a grade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<Grade>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    /// Tasks that must finish before this one. Added with the task: if one is refused, no task is created.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    /// Tasks this one blocks, added the same way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
}

/// `PATCH /v1/tasks/{task}`: a field left out is kept, and where it can be cleared, `null` (`Some(None)`) clears it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// 1 to 4. A human can set, raise, lower or `null` it; an agent can only raise an existing grade.
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    pub grade: Option<Option<Grade>>,
    /// Key of the task this one duplicates. Needs `status` `cancelled` (in this request or already);
    /// `null` clears it, and so does leaving `cancelled`.
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<Option<String>>,
    /// A new day without `due_at` drops the time; `null` removes the deadline.
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = Date))]
    pub due_on: Option<Option<String>>,
    /// Moves the day to match; `null` keeps the day without a time.
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub due_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    pub due_tz: Option<Option<String>>,
    /// A new day without `scheduled_at` drops the time; `null` unschedules.
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = Date))]
    pub scheduled_on: Option<Option<String>>,
    /// Moves the day to match; `null` keeps the day without a time.
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(format = DateTime))]
    pub scheduled_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    pub scheduled_tz: Option<Option<String>>,
    /// The key of the task to move this one under; `null` makes it top-level.
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    pub parent: Option<Option<String>>,
    /// Merged into the task's metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// A handoff comment written with the update. If the update is refused, it isn't written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<String>,
    /// Refuses the update if the task has changed since this version, like `If-Match`: accepted in the body for
    /// clients that can't set headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_tells_a_field_left_out_from_one_cleared_both_ways() {
        let cleared = UpdateTask { grade: Some(None), title: Some("New".into()), ..UpdateTask::default() };
        let sent = serde_json::to_value(&cleared).unwrap();
        assert_eq!(sent, serde_json::json!({ "title": "New", "grade": null }), "only what is set is sent");
        assert_eq!(serde_json::from_value::<UpdateTask>(sent).unwrap(), cleared, "and it reads back as it was");
        let kept: UpdateTask = serde_json::from_str("{}").unwrap();
        assert_eq!(kept, UpdateTask::default(), "a field left out is kept");
    }

    #[test]
    fn a_new_task_needs_only_its_project_and_title() {
        let new: CreateTask = serde_json::from_str(r#"{"project":"TRAIL","title":"T"}"#).unwrap();
        assert_eq!(new, CreateTask { project: "TRAIL".into(), title: "T".into(), ..CreateTask::default() });
        let sent = serde_json::to_value(&new).unwrap();
        assert_eq!(sent, serde_json::json!({ "project": "TRAIL", "title": "T", "description": "" }));
    }
}
