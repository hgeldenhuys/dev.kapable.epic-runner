use serde_json::json;
use tokio::sync::mpsc;

use crate::api_client::ApiClient;
use crate::types::SprintEvent;

/// Async event sink that streams ceremony events to the DB in real-time.
///
/// Architecture: sync `emit()` → mpsc channel → background tokio task → POST /v1/ceremony_events.
/// The platform's WAL→SSE pipeline auto-delivers events to any subscribed UI/observer.
/// This enables live scrum master intervention during ceremonies.
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::UnboundedSender<SprintEvent>,
}

impl EventSink {
    /// Spawn a background writer that POSTs events to /v1/ceremony_events.
    /// Returns the sink (Clone-able, pass to multiple nodes) and a join handle
    /// for the background writer (await after dropping the sink to flush).
    pub fn spawn(client: ApiClient) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<SprintEvent>();
        let handle = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let payload = json!({
                    "sprint_id": event.sprint_id.to_string(),
                    "event_type": event.event_type_str(),
                    "node_id": event.node_id,
                    "node_label": event.node_label,
                    "summary": event.summary,
                    "detail": event.detail,
                    "timestamp": event.timestamp.to_rfc3339(),
                });
                let result: Result<serde_json::Value, _> =
                    client.post("/v1/ceremony_events", &payload).await;
                if let Err(e) = result {
                    tracing::warn!(error = %e, "Failed to write ceremony event to DB");
                }
            }
        });
        (Self { tx }, handle)
    }

    /// No-op sink — events are silently dropped.
    /// Use for tests or when DB streaming is not configured.
    pub fn noop() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self { tx }
    }

    /// Emit an event. Non-blocking, best-effort.
    /// If the background writer is gone (noop or dropped), the event is silently discarded.
    pub fn emit(&self, event: SprintEvent) {
        let _ = self.tx.send(event);
    }
}
