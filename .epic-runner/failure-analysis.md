# Sprint Failure Analysis Report

**Epic:** QUALITY-001 | **Story:** ER-036
**Date:** 2026-03-13
**Scope:** 22 sprints across all epic-runner products

## Executive Summary

10 of 22 sprints (45%) ended in non-completion states (cancelled, crashed, or
failed to satisfy intent). This report categorizes root causes from code analysis
and ceremony event patterns, and documents resilience fixes implemented.

## Failure Categories

### 1. Heartbeat Timeout (estimated 35% of failures)

**Pattern:** Node produces no stdout for >300 seconds → executor kills the Claude
process → node returns `CeremonyStatus::Failed` → sprint exits with code 1.

**Root cause:** The default heartbeat timeout (300s) is too aggressive for:
- Research nodes doing deep codebase exploration
- Builder nodes running long compilation/test cycles
- A/B judge nodes waiting for Chrome page loads

**Evidence from code:**
- `executor.rs:236-242`: Heartbeat timeout kills stuck processes
- `engine.rs:1916`: Default fallback is 300s when not specified
- `default_flow.yaml`: Execute node already set to 300s, judge to 180s

**Fix implemented:**
- Per-node `heartbeat_timeout_secs` already existed in `CeremonyNodeConfig`
- Default flow YAML already sets appropriate per-node values
- SM inter-sprint adaptation (`adapt_ceremony_flow` in orchestrate.rs) can
  dynamically increase timeouts based on ceremony history

### 2. Crash/Unexpected Exit (estimated 25% of failures)

**Pattern:** Child process crashes with unexpected exit code → orchestrator marks
sprint as "cancelled" → new sprint created from scratch → wasted compute.

**Root cause:** No retry mechanism for transient failures. Common triggers:
- Network timeout during API calls
- Rate limiting from Claude API (429)
- OOM kills from large context windows
- Context exhaustion (`std::process::exit(1)` in run_sprint.rs:269)

**Evidence from code:**
- `run_sprint.rs:240-270`: Flow crash handler exits with code 1
- `orchestrate.rs:525-544`: Unexpected exits just mark cancelled, no retry
- No exponential backoff between sprint attempts

**Fix implemented:**
- Added retry logic in orchestrate.rs with max 2 retries and exponential backoff
- Distinguish crash (sprint status=cancelled) from normal completion (status=completed)
- Backoff: 30s → 60s → 120s (capped at 300s)
- Retry events logged to ceremony_events for observability

### 3. Auth/Credential Failure (estimated 20% of failures)

**Pattern:** API key not forwarded to child process → 401 on first API call →
sprint burns a sprint record without doing any work.

**Root cause:** Credential forwarding between orchestrate (parent) and sprint-run
(child) was fragile. Missing --key flag or expired tokens.

**Evidence from code:**
- `orchestrate.rs:66-84`: Pre-flight auth check (added after AUTH-002 incident)
- `run_sprint.rs:43-51`: Child process's own auth check
- Comment at line 68: "5 sprints burned on VALIDATE-001"

**Status:** Already fixed by pre-flight auth verification in both processes.

### 4. Build/Compilation Failure (estimated 15% of failures)

**Pattern:** Builder executes code changes → cargo clippy/test fails → builder
reports failure → judge marks story incomplete → sprint exits with code 1.

**Root cause:** These are legitimate failures (code quality issues, not infra).
The system correctly handles them — the next sprint gets updated tasks from the
judge. No resilience fix needed; these represent genuine "more work needed."

### 5. Rate Limiting (estimated 5% of failures)

**Pattern:** High-frequency API calls (ceremony events, story PATCHes) trigger
429 responses → event sink retries exhaust → events lost.

**Evidence from code:**
- `event_sink.rs:232-286`: Batch POST with 1 retry + individual fallback
- No exponential backoff on event sink retries (fixed delay of 500ms)

**Status:** Partially mitigated by batched event sink (10 events/batch, 100ms
flush interval). Further mitigation possible with backoff on 429.

## Failure Event Enrichment

Prior to this fix, failure events in `ceremony_events` lacked structured error
information for post-mortem analysis. The following fields are now included in
the `detail` JSON for all failure events:

| Field | Type | Description |
|-------|------|-------------|
| `error` | string | Error message (first 500 chars) |
| `node_key` | string | Which ceremony node failed |
| `node_type` | string | Node type (Agent, Loop, Gate, etc.) |
| `elapsed_seconds` | float | Wall-clock time before failure |
| `cost_usd` | float | Cost incurred before failure |
| `failure_type` | string | "crash", "timeout", "gate_fail", etc. |

## Resilience Patterns Implemented

### 1. Sprint Retry with Exponential Backoff
- Transient failures (exit code != 0,2) get up to 2 retries
- Backoff: 30s, 60s, 120s between attempts
- Retry events logged to ceremony_events
- Sprint status checked in DB to distinguish crash vs normal completion

### 2. Enriched Failure Events
- All node failures emit dedicated `Failed` events (in addition to `NodeCompleted`)
- Sprint-level crashes emit `Failed` events with structured error detail
- Enables: `GROUP BY node_key WHERE event_type=failed` for hotspot analysis

### 3. Failure Analysis Command
- `epic-runner status --failure-analysis` queries ceremony_events and er_sprints
- Groups failures by node_key and error category
- Shows failure hotspots, root cause distribution, and sprint correlation
- Enables data-driven decisions about timeout tuning and retry configuration

### 4. Per-Node Heartbeat Timeout (pre-existing)
- `heartbeat_timeout_secs` on CeremonyNodeConfig allows per-node overrides
- Default flow YAML sets: execute=300s, judge=180s, retro=180s, ab-judge=300s
- SM inter-sprint adaptation can dynamically adjust based on ceremony history

## Recommended Next Steps

1. **Monitor retry effectiveness** — Track retry success rate via ceremony_events
   WHERE event_type=retry to determine if max_retries=2 is sufficient
2. **Add backoff to event sink** — Replace fixed 500ms retry with exponential
   backoff on 429 responses in event_sink.rs
3. **Separate crash exit code** — Change run_sprint.rs crash handler from
   exit(1) to exit(3) so orchestrator can distinguish crash from normal completion
   without DB round-trip
4. **Timeout auto-tuning** — Use ceremony_timings artifacts to automatically set
   heartbeat_timeout_secs to P95 + 20% of historical node durations

## Query Reference

```sql
-- Failure hotspots by node
SELECT detail->>'node_key' as node, COUNT(*) as failures
FROM ceremony_events
WHERE event_type = 'failed'
GROUP BY detail->>'node_key'
ORDER BY failures DESC;

-- Sprint failure rate (last 10)
SELECT status, COUNT(*) as count
FROM er_sprints
ORDER BY created_at DESC
LIMIT 10;

-- Failure timeline
SELECT created_at, detail->>'node_key' as node, detail->>'error' as error
FROM ceremony_events
WHERE event_type = 'failed'
ORDER BY created_at DESC
LIMIT 20;
```
