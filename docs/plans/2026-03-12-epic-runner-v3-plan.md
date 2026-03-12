# Epic Runner v3 — Autonomous Product Delivery Engine

**Date:** 2026-03-12
**Author:** Claude (co-founder, product owner by delegation)
**Status:** PLANNING — approved by committee review
**North Star:** Claude takes an ambitious requirement and delivers it through structured, self-correcting sprint loops — without human babysitting.

## Design Principles (from founder, non-negotiable)

1. **Sprints never fail.** They complete with whatever work got done.
2. **Backlog-first.** Stories exist independently. Epics pull from the backlog.
3. **Context = capacity.** A sprint is bounded by context rot (1-2 compactions max), not time.
4. **Judge + retro always run.** No gates skip these. The learning loop is the moat.
5. **Agent orientation is expensive.** Minimize re-discovery with product briefs and research artifacts.
6. **No quick fixes.** Do things properly. This is the most important component of the platform.

## Primitives (Scrum-inspired, context-aware)

| Primitive | Role | Analogy |
|-----------|------|---------|
| **Product** | What's being built. Has a repo, brief, changelog. | The "project" |
| **Backlog** | Flat pool of independent stories. The WHAT. | Product backlog |
| **Epic** | A mission/WHY that pulls stories from backlog. | Initiative / OKR |
| **Sprint** | Bounded unit of work. The HOW. Capacity = context budget. | Sprint (2-3 context windows) |
| **Sprint Goal** | Focused subset of epic mission for this sprint. | Sprint goal |
| **Story** | Unit of deliverable work with ACs and tasks. | User story / PBI |
| **Sprint Assignment** | Links a story to a sprint. Can be deferred back to pool. | Sprint backlog item |
| **Research Artifact** | Pre-computed codebase intelligence. Injectable. Has staleness TTL. | Spike output |
| **Ceremony** | Structured YAML DAG process. Template + overrides. | Sprint events |
| **PRODUCTS.md** | Auto-generated agent orientation doc. Updated by SM after each retro. | Team wiki |

## Sprint Status Model

```
planning → executing → completed | cancelled | blocked
```

- **completed** = ceremony ran to conclusion (partial work is normal and expected)
- **cancelled** = externally interrupted (killed, context exhaustion, user abort)
- **blocked** = impediment raised, needs human input
- **NO "failed" status.** A sprint that didn't finish all stories is completed.

## Sprint Capacity Model

| Size | Context Fraction | Description |
|------|-----------------|-------------|
| XS | 1/8 window | Config change, one-liner fix |
| S | 1/4 window | Single function/component |
| M | 1/2 window | Module-level change |
| L | 1 window | Cross-module, needs research |
| XL | Break down | Too big for one sprint |

Sprint budget: ~2 context windows (allowing 1-2 compactions). Groomer uses story sizes to plan capacity.

## Exit Code Semantics (corrected)

| Code | Meaning | Orchestrator Action |
|------|---------|-------------------|
| 0 | Epic mission complete | Close epic, break loop |
| 1 | More work needed (NOT failed) | Create next sprint, pull new stories from backlog |
| 2 | Blocked by impediment | Pause epic, needs human |

## Ceremony Flow Changes

### Current (v2.3): Gates short-circuit
```
research → gate → groom → gate → execute → gate → judge → gate → deploy → gate → ab_judge → gate → promote → merge → retro → output
                                    ↓ fail                        ↓ fail
                              merge_results ←─────────────── merge_results
```

### New (v3): Gates annotate, judge + retro always run
```
research → groom → execute → code_judge(always) → deploy(conditional) → ab_judge(conditional) → retro(always) → output
                                  ↓                      ↓
                           annotates results       skipped if judge
                           for deploy decision     says not ready
```

Key changes:
- Remove gate nodes between research/groom/execute. These are sequential by nature.
- **code_judge** always runs — evaluates what was accomplished, not pass/fail.
- **deploy** is conditional on code_judge saying code is ready. If not ready, skip deploy + ab_judge.
- **retro** ALWAYS runs — generates learnings, velocity data, discovered work.
- Judge output includes: `mission_progress` (0-100%), `stories_completed`, `delta_stories` (new stories for backlog), `deploy_ready` (bool).

### Flow Template + Override Model
- Base flow: `src/flow/default_flow.yaml` (embedded, immutable at runtime)
- Product override: `.epic-runner/ceremony_overrides.yaml` (committed to git)
- SM can propose patches to override file in retro (versioned, auditable)
- Never mutate running flow mid-execution

## Data Model Changes

### New: backlog_items (replaces stories for new model)
Stories table stays for v2 compat. New backlog_items table for v3:
- `id`, `product_id`, `code`, `title`, `description`
- `acceptance_criteria` (JSON array of {criterion, testable_by})
- `tasks` (JSON array of {task, file_path, line_number})
- `size` (xs|s|m|l|xl)
- `tags` (JSON array — for groomer matching to epic mission)
- `status` (draft|refined|ready|done)
- `created_at`, `updated_at`
- NO `epic_code` — stories are independent

### New: sprint_assignments
- `id`, `sprint_id`, `backlog_item_id`
- `status` (assigned|in_progress|completed|deferred)
- `started_at`, `completed_at`
- `notes` (what was accomplished, even if partial)

### New: research_artifacts
- `id`, `product_id`, `epic_code` (optional — can be product-wide)
- `content` (JSON — files, patterns, dependencies, conventions)
- `staleness_ttl_sprints` (default 3 — refresh after 3 sprints)
- `created_at`, `refreshed_at`

### Modified: er_sprints
- Add `goal` field (sprint goal — focused subset of epic mission)
- Add `velocity` (JSON — {stories_planned, stories_completed, context_windows_used})
- Remove `failed` from SprintStatus enum
- Add `cancelled` to SprintStatus enum

### Modified: products
- Add `brief` field (PRODUCTS.md content — auto-generated)
- Add `changelog` field (JSON array of recent changes)
- Keep `repo_path` for now (portability fix is separate)

## Epic Sequencing

### Epic 1: CORE-001 — Sprint Integrity
**Mission:** Make sprints honest. A sprint always completes, the judge always evaluates, the retro always learns. No more "failed" sprints, no gate short-circuits skipping the learning loop.

**Why first:** Everything else depends on correct sprint semantics. Running more epics with the broken model wastes compute and produces misleading data.

**Stories (from backlog):**
1. Remove "failed" from SprintStatus, add "cancelled"
2. Change exit code 1 semantics: "more work needed" not "failed"
3. Rewrite ceremony flow v3: remove gate short-circuits, judge+retro always run
4. Judge outputs mission_progress + delta_stories (not just pass/fail)
5. Orchestrator: exit code 1 means "pull new stories" not "sprint failed"
6. cleanup_stale_sprints marks zombies as "cancelled" not "failed"

### Epic 2: BACKLOG-001 — Backlog-First Architecture
**Mission:** Stories exist independently in the backlog. Epics define missions. The groomer pulls stories from the backlog that serve the sprint goal. No pre-assignment bias.

**Why second:** The backlog-first model is the foundation of how work flows through the system. Without it, epics are just story containers.

**Stories:**
1. Create backlog_items table (independent of epics)
2. Create sprint_assignments table (links stories to sprints)
3. Groomer agent: search backlog for mission-aligned stories
4. Sprint goal primitive: focused subset of epic mission
5. Judge creates delta_stories back into backlog (not epic-scoped)
6. CLI: `epic-runner backlog add` creates items without epic
7. Orchestrator uses sprint_assignments, not stories field on sprint

### Epic 3: ORIENT-001 — Agent Orientation & Learning Loop
**Mission:** Cut agent orientation cost to near-zero. Every sprint starts with a product brief, recent changelog, and injectable research. Learnings compound across epics, not just sprints.

**Why third:** This is the highest ROI for compute efficiency. Every sprint that re-discovers the codebase wastes ~$2-3 in API calls.

**Stories:**
1. PRODUCTS.md auto-generation (SM updates after each retro)
2. Research artifacts table with staleness TTL
3. Research phase: runs once per epic, feeds all sprints
4. Changelog: auto-maintained from git log + retro output
5. Cross-epic learnings: sprint_learnings searchable across products
6. Inject PRODUCTS.md into all agent system prompts
7. Story sizing: T-shirt sizes mapped to context fractions
8. Velocity tracking: context-windows consumed vs stories completed

### Epic 4: VIS-001 — Sprint Visibility & Console UX
**Mission:** Make the console show everything an observer needs: ACs, tasks, sprint-story assignments, research output, cost aggregation, mission progress.

**Stories:**
1. ACs displayed per story in sprint view
2. Sprint-story assignment table shown in UI
3. Tasks within stories visible and checkable
4. Research artifacts surfaced in epic detail view
5. Sprint-level cost aggregation (from ceremony_log)
6. Mission progress bar (from judge's mission_progress %)
7. Sprint goal shown prominently in sprint header
8. Velocity chart across sprints

### Epic 5: INFRA-001 — Infrastructure Resilience
**Mission:** Console never 503s during deploys. Proxy retries gracefully. SSR falls back to client-side. Context budget tracked and respected.

**Stories:**
1. Proxy retry + circuit breaker for Connect App routing
2. SSR graceful degradation (client-side fallback on render abort)
3. Blue-green deploy health gate before traffic switch
4. Context budget tracking in executor (graceful sprint end)
5. repo_path portability (git remote URL + local resolution)
6. Dead PID detection in lock files
7. Default branch detection (not hardcoded "main")

## Migration Strategy

**Don't migrate old data.** (Committee recommendation)

- Add new tables (backlog_items, sprint_assignments, research_artifacts)
- Keep old tables alive (stories, epics with embedded stories)
- Orchestrator checks model version: presence of sprint_assignments = v3, else v2
- Old epics (UI-001 through HARDEN-002) stay on v2 model
- New epics use v3 model
- Six months from now, delete v2 code path

## Execution Order

```
CORE-001 (sprint integrity) ← DO FIRST, everything depends on this
    ↓
BACKLOG-001 (backlog-first) ← enables proper story management
    ↓
ORIENT-001 (agent orientation) ← highest ROI for compute efficiency
    ↓
VIS-001 + INFRA-001 (in parallel) ← polish and resilience
```

## Success Criteria

The epic runner can:
1. Run 5 consecutive sprints on itself without human intervention
2. Each sprint completes (never "fails")
3. Judge accurately evaluates mission progress and generates delta stories
4. Groomer pulls appropriate stories from a flat backlog
5. Agent orientation cost drops 80% (PRODUCTS.md + research injection)
6. Console shows full sprint lifecycle with ACs, tasks, costs, progress
7. No 503s during blue-green deploys
