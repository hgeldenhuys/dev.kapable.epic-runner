use serde_json::json;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{sleep, Instant};

use crate::api_client::ApiClient;
use crate::types::SprintEvent;

/// Maximum events per batch POST.
const BATCH_SIZE: usize = 10;
/// Maximum time to buffer before flushing (100ms).
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// Retry delay for transient failures.
const RETRY_DELAY: Duration = Duration::from_millis(500);

/// Async event sink that streams ceremony events to the DB in real-time.
///
/// Architecture: sync `emit()` → mpsc channel → background tokio task → batched POST /v1/ceremony_events.
/// Events are buffered for up to 100ms or 10 events (whichever comes first) to reduce HTTP overhead.
/// A busy sprint (200-6000+ events) now makes 20-600 HTTP calls instead of one per event.
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::UnboundedSender<SprintEvent>,
}

impl EventSink {
    /// Spawn a background writer that batches and POSTs events to /v1/ceremony_events.
    /// Returns the sink (Clone-able, pass to multiple nodes) and a join handle
    /// for the background writer (await after dropping the sink to flush).
    pub fn spawn(client: ApiClient) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<SprintEvent>();
        let handle = tokio::spawn(async move {
            let mut buffer: Vec<SprintEvent> = Vec::with_capacity(BATCH_SIZE);
            let mut flush_deadline: Option<Instant> = None;

            loop {
                let should_flush;

                if buffer.is_empty() {
                    // Nothing buffered — block until an event arrives or channel closes
                    match rx.recv().await {
                        Some(event) => {
                            buffer.push(event);
                            flush_deadline = Some(Instant::now() + FLUSH_INTERVAL);
                            should_flush = buffer.len() >= BATCH_SIZE;
                        }
                        None => break, // Channel closed — exit loop, flush below
                    }
                } else {
                    // We have buffered events — wait for more or timeout
                    let deadline =
                        flush_deadline.unwrap_or_else(|| Instant::now() + FLUSH_INTERVAL);
                    let remaining = deadline.saturating_duration_since(Instant::now());

                    tokio::select! {
                        event = rx.recv() => {
                            match event {
                                Some(e) => {
                                    buffer.push(e);
                                    should_flush = buffer.len() >= BATCH_SIZE;
                                }
                                None => {
                                    // Channel closed — flush remaining and exit
                                    if !buffer.is_empty() {
                                        flush_batch(&client, &mut buffer).await;
                                    }
                                    break;
                                }
                            }
                        }
                        _ = sleep(remaining) => {
                            should_flush = true;
                        }
                    }
                }

                if should_flush && !buffer.is_empty() {
                    flush_batch(&client, &mut buffer).await;
                    flush_deadline = None;
                }
            }

            // Final flush for any remaining events
            if !buffer.is_empty() {
                flush_batch(&client, &mut buffer).await;
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

/// Flush a batch of events via POST. Retries once on transient failure.
async fn flush_batch(client: &ApiClient, buffer: &mut Vec<SprintEvent>) {
    let payloads: Vec<serde_json::Value> = buffer
        .iter()
        .map(|event| {
            json!({
                "sprint_id": event.sprint_id.to_string(),
                "event_type": event.event_type_str(),
                "node_id": event.node_id,
                "node_label": event.node_label,
                "summary": event.summary,
                "detail": event.detail,
                "timestamp": event.timestamp.to_rfc3339(),
            })
        })
        .collect();

    let batch_size = payloads.len();

    // Try batch POST first (array payload)
    let result: Result<serde_json::Value, _> = client
        .post(
            "/v1/ceremony_events",
            &serde_json::Value::Array(payloads.clone()),
        )
        .await;

    match result {
        Ok(_) => {
            tracing::debug!(batch_size, "Flushed ceremony event batch");
        }
        Err(first_err) => {
            tracing::warn!(error = %first_err, batch_size, "Batch POST failed, retrying once");
            sleep(RETRY_DELAY).await;

            // Retry once
            let retry: Result<serde_json::Value, _> = client
                .post(
                    "/v1/ceremony_events",
                    &serde_json::Value::Array(payloads.clone()),
                )
                .await;

            match retry {
                Ok(_) => {
                    tracing::debug!(batch_size, "Batch retry succeeded");
                }
                Err(retry_err) => {
                    tracing::error!(
                        error = %retry_err,
                        batch_size,
                        "Batch POST failed after retry — falling back to individual POSTs"
                    );
                    // Fall back to individual POSTs
                    for payload in &payloads {
                        let individual: Result<serde_json::Value, _> =
                            client.post("/v1/ceremony_events", payload).await;
                        if let Err(e) = individual {
                            tracing::warn!(error = %e, "Individual ceremony event POST also failed");
                        }
                    }
                }
            }
        }
    }

    buffer.clear();
}
