use anyhow::Result;
use uuid::Uuid;

use super::Ctx;
use crate::client::api_err;
use crate::output::text;

/// Lists the notifications, or marks one or all of them read.
pub(crate) async fn inbox(cx: &Ctx, all: bool, limit: i64, read: Option<Uuid>, read_all: bool) -> Result<()> {
    let api = cx.c.api();
    if let Some(id) = read {
        let n = api.mark_notification(id, true).await.map_err(api_err)?;
        cx.emit(&n, |n| text().notification(n));
        return Ok(());
    }
    if read_all {
        let count = api.mark_all_notifications_read().await.map_err(api_err)?;
        cx.emit(&serde_json::json!({ "count": count }), |_| format!("marked {count} read"));
        return Ok(());
    }
    let list = api.list_notifications(!all, None, limit).await.map_err(api_err)?;
    cx.emit(&list, |list| match list.notifications.as_slice() {
        [] if all => "nothing has been raised".to_string(),
        [] => "nothing unread".to_string(),
        ns => ns.iter().map(|n| text().notification(n)).collect::<Vec<_>>().join("\n"),
    });
    Ok(())
}
