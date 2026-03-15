# Epic Runner

Rust CLI for epic-scoped autonomous sprint execution on the Kapable platform.

## Architecture

**Two execution engines (selectable via `--engine`):**

### Pipeline Engine (default: `--engine=pipeline`)
- `epic-runner orchestrate <EPIC_CODE>` — generates PipelineDefinition, submits to platform API, polls for completion
- `epic-runner pipeline generate <EPIC_CODE>` — outputs pipeline YAML to stdout for inspection/modification
- `epic-runner pipeline submit <FILE>` — submits a YAML file to the pipeline API
- Uses `AgentStepRunner` in `kapable-pipeline` crate for Claude CLI dispatch
- Platform handles: DAG execution, event streaming, persistence, crash recovery
- Agent steps get `hooks_settings` (stop-gate, track-files) via `--settings` flag

### Ceremony Engine (legacy: `--engine=ceremony`)
- `epic-runner sprint-run <SPRINT_ID>` — fat executor: loads ceremony YAML DAG, executes nodes via Kahn's BFS
- Ceremony-as-data: Sprint ceremonies defined in YAML DAGs (`src/flow/default_flow.yaml`)
- 9 node types: Source, Harness, Agent, Gate, Loop, Merge, Output, Deploy, Promote
- Will be removed once pipeline engine is fully validated

**Entity management:** Products, epics, stories, sprints, research, impediments — same regardless of engine.

## Project Structure

```
src/
  main.rs              # CLI entry (clap derive)
  lib.rs               # Library crate (all modules)
  agents.rs            # Embedded agent definitions (include_str! + temp dir resolution)
  api_client.rs        # Kapable Data API client (x-api-key auth, Clone)
  event_sink.rs        # Real-time ceremony event streaming (mpsc → DB)
  config.rs            # TOML config with project walk
  types.rs             # Domain types (Epic, Sprint, Story, etc.)
  executor.rs          # Claude Code subprocess dispatch (ceremony engine)
  stream.rs            # stream-json line parser
  supervisor.rs        # Stop-hook loop + rubber duck (ceremony engine)
  pipeline_generator.rs # Generate PipelineDefinition from sprint context (pipeline engine)
  pipeline_submitter.rs # Submit pipeline to API + poll for completion (pipeline engine)
  builder.rs           # Builder output parsing + story write-back pipeline
  judge.rs             # Verdict parsing + confidence threshold
  scrum_master.rs      # Retrospective output parsing
  impediments.rs       # Cross-epic blocker queries
  flow/
    definition.rs      # CeremonyFlow, CeremonyNode types
    default_flow.yaml  # 12-node v4 ceremony DAG (no research/groom — stories arrive pre-groomed) (embedded via include_str!)
    engine.rs          # Kahn's BFS executor with gate skipping
    loader.rs          # Flow loading cascade (file → config → embedded)
  commands/
    mod.rs             # Command enum + dispatch
    init.rs            # Project provisioning (tables + config)
    product.rs         # Product CRUD
    backlog.rs         # Story CRUD (add/list/show/transition/delete)
    epic.rs            # Epic CRUD (create/list/show/close/abandon)
    sprint.rs          # Sprint list/show
    orchestrate.rs     # Thin supervisor (sprint loop)
    run_sprint.rs      # Fat executor (ceremony flow)
    review.rs          # Business review (standalone)
    retro.rs           # Retrospective (standalone)
    impediment.rs      # Impediment management
    status.rs          # Dashboard
agents/                  # Claude Code agent definitions (embedded in binary)
  researcher.md        # Read-only codebase research (sonnet)
  groomer.md           # Story grooming with DoR (sonnet)
  builder.md           # Sprint execution (opus)
  code-judge.md        # Code quality review (sonnet)
  ab-judge.md          # Chrome-based A/B comparison (sonnet)
  scrum-master.md      # Retrospective analysis (sonnet)
  rubber-duck.md       # Stuck-state debugging (haiku)
hooks/
  stop-gate.sh         # Stop hook: blocks session end until tasks complete
  track-files.sh       # PostToolUse hook: tracks changed files per story
tests/                 # Integration tests
```

## Config Cascade

API URL + key resolution: CLI flag → env var → project config → home config → default.

Project config: `.epic-runner/config.toml` (walks up from CWD to `.git` boundary).

## Stop Hook CLI Commands

The stop hook blocks builder sessions until tasks are done. Builders mark progress via CLI:

```bash
epic-runner backlog task-done <STORY_CODE> <INDEX>    # Mark task as done (0-based index)
epic-runner backlog ac-verify <STORY_CODE> <INDEX>    # Mark AC as verified
epic-runner backlog block <STORY_CODE> --reason "..."  # Flag story as blocked (escape hatch)
```

These update both the API and local `.epic-runner/stories/{uuid}.json` (for stop hook).
Max stop iterations: 3 (configurable via `EPIC_RUNNER_MAX_STOP_ITERATIONS`).

## Exit Code Protocol

`sprint-run` communicates outcome to `orchestrate` via process exit codes:
- `0` = intent satisfied (close epic)
- `1` = failed but not blocked (retry next sprint)
- `2` = blocked by impediment (pause epic)

## Build & Test

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo fmt --all -- --check
cargo build --release
```

## Key Dependencies

- `clap` — CLI argument parsing (derive)
- `reqwest` — HTTP client for Kapable Data API
- `serde_yaml` — Ceremony flow parsing
- `comfy-table` — Terminal table rendering
- `scopeguard` — Lock file cleanup
- `chrono` — Timestamps
- `uuid` — Session/sprint IDs

## Data API

Uses Kapable Data API with `x-api-key` header. **Key-scoped routing** — the API key determines which project's data you access. All routes are `/v1/{table_name}`, NOT `/v1/data/{project_id}/...`.

Tables provisioned by `epic-runner init`: products, stories, epics, **er_sprints** (not `sprints` — platform route collision with agentboard), impediments, supervisor_decisions, rubber_duck_sessions, **ceremony_events** (real-time streaming via WAL→SSE).

**Response shapes:**
- `GET /v1/{table}` (list) → `{"data": [...], "pagination": {...}}` — use `DataWrapper<Vec<Value>>`
- `GET /v1/{table}/{id}` (single) → bare JSON object
- `POST /v1/{table}` (create) → bare JSON object
- `PATCH /v1/{table}/{id}` (update) → bare JSON object
- `PUT /v1/_meta/tables/{name}` (DDL) → bare JSON object

**Auth tiers:**
- `sk_admin_*` — management operations (create projects, DDL)
- `sk_live_*` — project-scoped data operations (CRUD on tables)

## Claude Headless Integration

Sprint ceremonies dispatch Claude Code via subprocess (`claude -p --output-format stream-json`). The executor parses stream-json events, tracks cost, and handles stop-hook loops with supervisor escalation.

## Autonomous Operation — READ THIS

**STOP. Before doing ANYTHING on epic-runner, read `.epic-runner/overnight-ops.md`.**

That file is your operations manual. It defines your role (PO + Orchestrator),
your loop, your failure modes, and what "done" actually means.

Key rules (details in overnight-ops.md):
- **Use epic-runner to build epic-runner.** Do not write code manually. Create stories, kick off epics.
- **Never declare code "done" without running `epic-runner orchestrate`.** Unit tests ≠ done.
- **Never stop.** When one epic finishes, start the next. Create new work if the queue is empty.
- **After compaction: re-read `.epic-runner/overnight-ops.md` IMMEDIATELY.**
- **Establish `/loop 10m`** — the loop reads overnight-ops.md each cycle. It is the heartbeat.
- **Audio feedback** — `/speak` every status change, save MP3s to `/tmp/radio-claude/`.
