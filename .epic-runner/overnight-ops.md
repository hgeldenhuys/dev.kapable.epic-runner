# Overnight Operations Manual — Radio Claude

**Created:** 2026-03-12 ~midnight
**Session:** calm-cat (earnest-mantis continued)
**Authority:** Full autonomy granted by co-founder. "You are the captain now."

## Mission

Execute Epic Runner v3 plan: fix sprint model, implement backlog-first architecture,
add agent orientation, improve visibility and infrastructure. The epic runner builds itself.

## Plan Location

`docs/plans/2026-03-12-epic-runner-v3-plan.md` — READ THIS FIRST after any compaction.

## Epic Sequence

1. **CORE-001** — Sprint Integrity (sprints never fail, judge+retro always run)
2. **BACKLOG-001** — Backlog-First Architecture (stories independent, epics pull from pool)
3. **ORIENT-001** — Agent Orientation (PRODUCTS.md, research artifacts, changelogs)
4. **VIS-001** + **INFRA-001** — Visibility + Resilience (parallel)

## Design Principles (NON-NEGOTIABLE)

1. Sprints NEVER fail. They complete with whatever work got done.
2. Backlog-first. Stories exist independently. Epics pull from the backlog.
3. Context = capacity. 1-2 compactions max per sprint.
4. Judge + retro ALWAYS run. The learning loop is the moat.
5. No quick fixes. Do things properly.
6. Use /committee regularly for course correction.

## Loop Responsibilities (every 10 minutes)

1. **Check epic-runner orchestrate process** — is it running? stuck? finished?
2. **Audio status report** — save to `/tmp/radio-claude/` as timestamped MP3
3. **If stuck** — diagnose, fix infrastructure or code, restart
4. **If epic complete** — commit, push, speak results, start next epic
5. **If between epics** — invoke /committee for guidance, create new stories if needed
6. **If context getting heavy** — use `/compact` with aggressive summary, then re-read this file
7. **Check /chrome** periodically — verify console UI renders correctly

## Audio Log Protocol

Save ALL audio updates to `/tmp/radio-claude/` with timestamps:
```bash
mkdir -p /tmp/radio-claude
# Filename format: YYYY-MM-DD_HHMMSS_topic.mp3
# e.g., /tmp/radio-claude/2026-03-12_234500_core001-sprint1-research.mp3
```

Play via afplay AND save the file. User will listen to chronicles in the morning.

## Model Selection (from compass article)

- **This session (orchestrator):** Opus — sustained reasoning, coordination
- **Ceremony nodes:** Already configured per-node in default_flow.yaml
  - researcher/groomer/scrum-master/code-judge/ab-judge: Sonnet (cheaper, 97-99% as good)
  - builder: Opus (complex multi-file work)
- **Effort levels matter more than model choice** — use --effort high for critical nodes

## Key Files

| File | Purpose |
|------|---------|
| `docs/plans/2026-03-12-epic-runner-v3-plan.md` | The v3 plan (north star) |
| `.epic-runner/config.toml` | API keys, project config |
| `src/flow/default_flow.yaml` | Ceremony DAG (v2.3, to be replaced with v3) |
| `src/commands/orchestrate.rs` | Sprint loop |
| `src/commands/run_sprint.rs` | Sprint execution + exit codes |
| `src/flow/engine.rs` | Flow engine + gate handling |
| `src/types.rs` | Domain types (SprintStatus, etc.) |
| `memory/epic-runner-design-philosophy.md` | Core design principles |

## API Keys

- Data API: see `.epic-runner/config.toml` (sk_live_* key)
- Admin API: see `.epic-runner/config.toml` (sk_admin_* key)
- Deploy: use /deploy-kapable skill for Rust, Connect App Pipeline for frontends

## Console App

- URL: https://console.kapable.dev
- App ID: `9ee900e7-3d10-46f1-b59b-bade220cfaa4`
- Deploy: `POST /v1/apps/{id}/environments/production/deploy` with x-api-key header

## Emergency Procedures

- **503 on console**: Check `ssh kapable-prod` container logs, restart if needed
- **Stale sprints**: PATCH status to "completed" (NOT "failed"), set finished_at
- **Stuck orchestrate**: Kill process tree, mark sprint as "cancelled", restart
- **Context exhaustion**: `/compact` with full summary, re-read this file, continue

## Compaction Template

When compacting, include this in the description:
```
Overnight epic-runner v3 execution. Read .epic-runner/overnight-ops.md for full context.
Current epic: {EPIC_CODE}. Current sprint: {N}. Status: {what's happening}.
Plan: docs/plans/2026-03-12-epic-runner-v3-plan.md. Design: memory/epic-runner-design-philosophy.md.
```

## Process Rules

- Commit after each meaningful change (not mid-work)
- Push after each commit (epic-runner repo has its own remote)
- Run `cargo clippy --workspace -- -D warnings && cargo test && cargo fmt --all -- --check` before committing
- Use /committee every 2-3 epics for course correction
- Save audio log for every status update
- Check console UI with /chrome after deploying frontend changes
