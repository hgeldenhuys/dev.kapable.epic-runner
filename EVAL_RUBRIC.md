# Epic Runner — Mechanism Scoring Rubric

Dogfood evaluation rubric for the Epic Runner CLI, proving end-to-end functionality against real Kapable infrastructure.

**Scoring**: 10 mechanisms, 0-3 points each, 30 total.

---

## M1: Init Provisioning (0-3)

| Score | Criteria |
|-------|----------|
| 0 | `epic-runner init` fails or errors |
| 1 | Command runs but some tables fail to create |
| 2 | All 9 tables created but config file missing |
| 3 | All 9 tables created (`products`, `stories`, `epics`, `er_sprints`, `impediments`, `supervisor_decisions`, `rubber_duck_sessions`, `ceremony_events`, `sprint_learnings`) AND `.epic-runner/config.toml` written |

**Auto-score**: Check table count via API (`curl /v1/_meta/tables`), verify config file exists.

---

## M2: Product CRUD (0-3)

| Score | Criteria |
|-------|----------|
| 0 | Product create fails |
| 1 | Product creates but list/show broken |
| 2 | Create + list work, `story_prefix` not auto-derived |
| 3 | Create, list, show all work. `story_prefix` auto-derived from slug (`"epic-runner"` -> `"ER"`) |

**Auto-score**: Parse `epic-runner product list --json` output.

---

## M3: Story Codes (0-3)

| Score | Criteria |
|-------|----------|
| 0 | Story add fails |
| 1 | Stories created but no code assigned |
| 2 | Codes assigned but not sequential |
| 3 | Codes are sequential per-product (`ER-001`, `ER-002`...), code lookup works in show/transition/delete |

**Auto-score**: Parse `epic-runner backlog list --json` output, verify sequential codes.

---

## M4: Epic Creation (0-3)

| Score | Criteria |
|-------|----------|
| 0 | Epic create fails |
| 1 | Epic creates but code format wrong |
| 2 | Epic creates with correct `DOMAIN-NNN` code |
| 3 | Create, list, show, close all work correctly |

**Auto-score**: Parse `epic-runner epic list --json` output.

---

## M5: Sprint Creation + Orchestrate (0-3)

| Score | Criteria |
|-------|----------|
| 0 | Sprint creation or orchestrate launch fails |
| 1 | Sprint created but orchestrate can't find it |
| 2 | Orchestrate launches sprint-run subprocess |
| 3 | Orchestrate creates sprint, assigns stories, launches sprint-run, reads exit code |

**Auto-score**: Requires observing a live sprint run.

---

## M6: Ceremony Flow Execution (0-3)

| Score | Criteria |
|-------|----------|
| 0 | Flow doesn't load or parse |
| 1 | First node executes but flow crashes mid-way |
| 2 | Most nodes complete but some fail |
| 3 | All 11 nodes complete (`source` -> `research` -> `gate` -> `groom` -> `gate` -> `execute` -> `gate` -> `judge` -> `merge` -> `retro` -> `output`) |

**Auto-score**: Requires observing a live sprint run.

---

## M7: Event Streaming (0-3)

| Score | Criteria |
|-------|----------|
| 0 | No events written to `ceremony_events` table |
| 1 | Sprint start event written but no node events |
| 2 | Start + some node events written |
| 3 | Full event stream: start, `node_started`/`completed` for each node, final `completed`/`failed` event. Events appear in real-time via SSE. |

**Auto-score**: Requires observing a live sprint run.

---

## M8: Sprint Outcome (0-3)

| Score | Criteria |
|-------|----------|
| 0 | Sprint crashes without updating DB |
| 1 | Sprint finishes but wrong status written to DB |
| 2 | Correct status + `ceremony_log` written |
| 3 | Correct status, `ceremony_log`, structured metrics emitted, exit code matches outcome (`0`/`1`/`2`) |

**Auto-score**: Requires observing a live sprint run.

---

## M9: Retro Feedback Loop (0-3)

| Score | Criteria |
|-------|----------|
| 0 | No retro output or learnings saved |
| 1 | Retro runs but learnings not persisted to `sprint_learnings` |
| 2 | Learnings persisted but not loaded in next sprint |
| 3 | Full loop: retro -> `sprint_learnings` -> `{{previous_learnings}}` template variable populated in next sprint's execute prompt |

**Auto-score**: Requires observing a live sprint run (two consecutive sprints).

---

## M10: Status Dashboard (0-3)

| Score | Criteria |
|-------|----------|
| 0 | `epic-runner status` crashes |
| 1 | Shows product but no epic data |
| 2 | Shows epics + stories but no cost/progress |
| 3 | Full dashboard: progress bars, cost tracking, sprint history icons, impediment display |

**Auto-score**: Run `epic-runner status --json` and check fields.

---

## Grading Scale

| Grade | Score | Verdict |
|-------|-------|---------|
| **A** | 27-30/30 | Production ready, ship it |
| **B** | 22-26/30 | Core works, polish needed (minimum pass) |
| **C** | 16-21/30 | Significant gaps, needs another sprint |
| **D** | 10-15/30 | Fundamental issues |
| **F** | 0-9/30 | Not functional |

---

## Auto-Scoring Summary

| Mechanism | Auto-Scorable | Method |
|-----------|---------------|--------|
| M1 | Yes | Check table count via API, verify config file |
| M2 | Yes | Parse `epic-runner product list --json` |
| M3 | Yes | Parse `epic-runner backlog list --json`, verify sequential codes |
| M4 | Yes | Parse `epic-runner epic list --json` |
| M5 | No | Observe live sprint run |
| M6 | No | Observe live sprint run |
| M7 | No | Observe live sprint run |
| M8 | No | Observe live sprint run |
| M9 | No | Observe live sprint run (two consecutive sprints) |
| M10 | Yes | Run `epic-runner status --json` and check fields |

M1-M4 and M10 (15 points) can be fully automated. M5-M9 (15 points) require observing a live sprint run.

---

## Prerequisites

- Kapable Data API running and accessible
- Valid API key (`sk_live_*` for data, `sk_admin_*` for init)
- `.epic-runner/config.toml` configured with API URL + key
- Claude Code installed (for ceremony execution)
