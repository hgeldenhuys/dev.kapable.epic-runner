use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{sleep, Instant};
use uuid::Uuid;

use crate::api_client::ApiClient;
use crate::flow::engine::NodeResult;
use crate::types::SprintEvent;

/// Maximum events per batch POST.
const BATCH_SIZE: usize = 10;
/// Maximum time to buffer before flushing (100ms).
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// Retry delay for transient failures.
const RETRY_DELAY: Duration = Duration::from_millis(500);

/// Lightweight metrics for EventSink — no external crate needed.
/// Tracks events sent, batches flushed, and batch size distribution.
#[derive(Debug, Default)]
pub struct EventSinkMetrics {
    pub events_sent: AtomicU64,
    pub batches_sent: AtomicU64,
    pub batch_size_sum: AtomicU64,
    pub batch_size_max: AtomicU64,
    pub individual_fallbacks: AtomicU64,
}

/// Async event sink that streams ceremony events to the DB in real-time.
///
/// Architecture: sync `emit()` → mpsc channel → background tokio task → batched POST /v1/ceremony_events.
/// Events are buffered for up to 100ms or 10 events (whichever comes first) to reduce HTTP overhead.
/// A busy sprint (200-6000+ events) now makes 20-600 HTTP calls instead of one per event.
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::UnboundedSender<SprintEvent>,
    metrics: Arc<EventSinkMetrics>,
}

impl EventSink {
    /// Spawn a background writer that batches and POSTs events to /v1/ceremony_events.
    /// Returns the sink (Clone-able, pass to multiple nodes) and a join handle
    /// for the background writer (await after dropping the sink to flush).
    pub fn spawn(client: ApiClient) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<SprintEvent>();
        let metrics = Arc::new(EventSinkMetrics::default());
        let task_metrics = Arc::clone(&metrics);
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
                                        flush_batch(&client, &task_metrics, &mut buffer).await;
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
                    flush_batch(&client, &task_metrics, &mut buffer).await;
                    flush_deadline = None;
                }
            }

            // Final flush for any remaining events
            if !buffer.is_empty() {
                flush_batch(&client, &task_metrics, &mut buffer).await;
            }

            // Log metrics summary at shutdown
            let events = task_metrics.events_sent.load(Ordering::Relaxed);
            let batches = task_metrics.batches_sent.load(Ordering::Relaxed);
            let max_batch = task_metrics.batch_size_max.load(Ordering::Relaxed);
            let fallbacks = task_metrics.individual_fallbacks.load(Ordering::Relaxed);
            let avg_batch = if batches > 0 {
                task_metrics.batch_size_sum.load(Ordering::Relaxed) as f64 / batches as f64
            } else {
                0.0
            };
            tracing::info!(
                events_sent = events,
                batches_sent = batches,
                avg_batch_size = format!("{:.1}", avg_batch),
                max_batch_size = max_batch,
                individual_fallbacks = fallbacks,
                "EventSink shutdown — metrics summary"
            );
        });
        (Self { tx, metrics }, handle)
    }

    /// No-op sink — events are silently dropped.
    /// Use for tests or when DB streaming is not configured.
    pub fn noop() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self {
            tx,
            metrics: Arc::new(EventSinkMetrics::default()),
        }
    }

    /// Return a snapshot of the current metrics (for sprint-level cost reporting).
    pub fn metrics(&self) -> &Arc<EventSinkMetrics> {
        &self.metrics
    }

    /// Emit an event. Non-blocking, best-effort.
    /// If the background writer is gone (noop or dropped), the event is silently discarded.
    pub fn emit(&self, event: SprintEvent) {
        let _ = self.tx.send(event);
    }

    /// Finalize a structured artifact from node completion.
    /// Unlike emit() which is fire-and-forget, this awaits the POST to ensure artifacts are persisted.
    /// Takes the ApiClient as a parameter since EventSink only holds the mpsc sender.
    #[allow(clippy::too_many_arguments)]
    pub async fn finalize_artifact(
        &self,
        client: &ApiClient,
        sprint_id: Uuid,
        epic_code: &str,
        artifact_type: &str,
        node_key: &str,
        title: &str,
        summary: Option<&str>,
        content: serde_json::Value,
    ) {
        let payload = json!({
            "sprint_id": sprint_id.to_string(),
            "epic_code": epic_code,
            "artifact_type": artifact_type,
            "node_key": node_key,
            "title": title,
            "summary": summary,
            "content": content,
        });

        match client
            .post::<_, serde_json::Value>("/v1/sprint_artifacts", &payload)
            .await
        {
            Ok(_) => tracing::info!(artifact_type, node_key, "Artifact finalized"),
            Err(e) => {
                tracing::warn!(error = %e, artifact_type, node_key, "Failed to finalize artifact")
            }
        }
    }
}

/// Flush a batch of events via POST. Retries once on transient failure.
/// Updates metrics counters for observability.
async fn flush_batch(
    client: &ApiClient,
    metrics: &EventSinkMetrics,
    buffer: &mut Vec<SprintEvent>,
) {
    let payloads: Vec<serde_json::Value> = buffer
        .iter()
        .map(|event| {
            json!({
                "sprint_id": event.sprint_id.to_string(),
                "event_type": event.event_type_str(),
                "node_id": event.node_id,
                "node_key": event.node_id,
                "node_label": event.node_label,
                "summary": event.summary,
                "detail": event.detail,
                "cost_usd": event.cost_usd,
                "timestamp": event.timestamp.to_rfc3339(),
            })
        })
        .collect();

    let batch_size = payloads.len() as u64;

    // Try batch POST first (array payload)
    let result: Result<serde_json::Value, _> = client
        .post(
            "/v1/ceremony_events",
            &serde_json::Value::Array(payloads.clone()),
        )
        .await;

    match result {
        Ok(_) => {
            metrics.events_sent.fetch_add(batch_size, Ordering::Relaxed);
            metrics.batches_sent.fetch_add(1, Ordering::Relaxed);
            metrics
                .batch_size_sum
                .fetch_add(batch_size, Ordering::Relaxed);
            metrics
                .batch_size_max
                .fetch_max(batch_size, Ordering::Relaxed);
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
                    metrics.events_sent.fetch_add(batch_size, Ordering::Relaxed);
                    metrics.batches_sent.fetch_add(1, Ordering::Relaxed);
                    metrics
                        .batch_size_sum
                        .fetch_add(batch_size, Ordering::Relaxed);
                    metrics
                        .batch_size_max
                        .fetch_max(batch_size, Ordering::Relaxed);
                    tracing::debug!(batch_size, "Batch retry succeeded");
                }
                Err(retry_err) => {
                    tracing::error!(
                        error = %retry_err,
                        batch_size,
                        "Batch POST failed after retry — falling back to individual POSTs"
                    );
                    metrics.individual_fallbacks.fetch_add(1, Ordering::Relaxed);
                    // Fall back to individual POSTs
                    for payload in &payloads {
                        let individual: Result<serde_json::Value, _> =
                            client.post("/v1/ceremony_events", payload).await;
                        match individual {
                            Ok(_) => {
                                metrics.events_sent.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Individual ceremony event POST also failed");
                            }
                        }
                    }
                    // Count the batch even on fallback
                    metrics.batches_sent.fetch_add(1, Ordering::Relaxed);
                    metrics
                        .batch_size_sum
                        .fetch_add(batch_size, Ordering::Relaxed);
                    metrics
                        .batch_size_max
                        .fetch_max(batch_size, Ordering::Relaxed);
                }
            }
        }
    }

    buffer.clear();
}

/// Extract artifact type and metadata from a completed node result.
/// Returns None if the node doesn't produce a meaningful artifact.
/// Returns (artifact_type, title, content) for nodes that do.
pub(crate) fn extract_artifact_info(
    node_key: &str,
    result: &NodeResult,
) -> Option<(&'static str, String, serde_json::Value)> {
    // Map node keys to artifact types
    let (artifact_type, title) = match node_key {
        "researcher" | "research" => ("research", "Sprint Research"),
        "groomer" | "groom" => ("acceptance_criteria", "Acceptance Criteria"),
        "judge" | "business_review" => ("judge_verdict", "Judge Verdict"),
        "retro" | "retrospective" | "sm_retro" => ("retrospective", "Sprint Retrospective"),
        _ => return None, // Not all nodes produce artifacts
    };

    // Build content JSONB from the node result
    let mut content = json!({
        "schema_version": "1",
        "status": format!("{:?}", result.status),
        "cost_usd": result.cost_usd,
    });

    // Add node output as the main content
    if let Some(ref output) = result.output {
        content["output"] = serde_json::Value::String(output.clone());
    }

    // Add judge verdict if present
    if let Some(ref verdict) = result.judge_verdict {
        content["verdict"] = serde_json::to_value(verdict).unwrap_or_default();
    }

    // Add supervisor decisions if any
    if !result.supervisor_decisions.is_empty() {
        content["supervisor_decisions"] =
            serde_json::to_value(&result.supervisor_decisions).unwrap_or_default();
    }

    // Add rubber duck sessions if any
    if !result.rubber_duck_sessions.is_empty() {
        content["rubber_duck_sessions"] =
            serde_json::to_value(&result.rubber_duck_sessions).unwrap_or_default();
    }

    Some((artifact_type, title.to_string(), content))
}
