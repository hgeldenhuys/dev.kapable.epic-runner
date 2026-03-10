# Epic Runner

Rust CLI for epic-scoped autonomous sprint execution on the Kapable platform.

## Architecture

**Dual-mode binary:**
- `epic-runner orchestrate <EPIC_CODE>` — thin supervisor: creates sprints, assigns stories, spawns `sprint-run` as child process, reads exit codes
- `epic-runner sprint-run <SPRINT_ID>` — fat executor: loads ceremony YAML DAG, executes nodes via Kahn's BFS, dispatches Claude headless, writes results to DB

**Ceremony-as-data:** Sprint ceremonies are defined in YAML DAGs (`src/flow/default_flow.yaml`), not hardcoded Rust. The flow engine supports 7 node types: Source, Harness, Agent, Gate, Loop, Merge, Output.

## Project Structure

```
src/
  main.rs              # CLI entry (clap derive)
  lib.rs               # Library crate (all modules)
  api_client.rs        # Kapable Data API client (x-api-key auth)
  config.rs            # TOML config with project walk
  types.rs             # Domain types (Epic, Sprint, Story, etc.)
  executor.rs          # Claude Code subprocess dispatch
  stream.rs            # stream-json line parser
  supervisor.rs        # Stop-hook loop + rubber duck
  judge.rs             # Verdict parsing + confidence threshold
  scrum_master.rs      # Retrospective output parsing
  impediments.rs       # Cross-epic blocker queries
  flow/
    definition.rs      # CeremonyFlow, CeremonyNode types
    default_flow.yaml  # 11-node ceremony DAG (embedded via include_str!)
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
agents/
  rubber-duck.md       # Stuck-state debugging agent (haiku)
tests/                 # Integration tests
```

## Config Cascade

API URL + key resolution: CLI flag → env var → project config → home config → default.

Project config: `.epic-runner/config.toml` (walks up from CWD to `.git` boundary).

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

Uses Kapable Data API (`/v1/data/{project_id}/...`) with `x-api-key` header. Tables provisioned by `epic-runner init`: products, stories, epics, sprints, impediments, supervisor_decisions, rubber_duck_sessions.

## Claude Headless Integration

Sprint ceremonies dispatch Claude Code via subprocess (`claude -p --output-format stream-json`). The executor parses stream-json events, tracks cost, and handles stop-hook loops with supervisor escalation.
