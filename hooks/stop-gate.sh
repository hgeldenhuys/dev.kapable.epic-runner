#!/bin/bash
# Stop hook — enforces task/AC completion before allowing a story session to end.
#
# Flow:
#   1. Read session ID from hook input
#   2. Check if session ID is in the cached story UUIDs (fast local check)
#   3. If not a story session → allow stop (exit 0)
#   4. If story session → read story JSON and validate:
#      a. All tasks marked done, OR
#      b. Story flagged as blocked with a reason
#   5. If validation fails → block stop with CLI instructions (exit 1)
#   6. If validation passes → generate git commit for the work done (exit 0)
#
# Environment variables (set by epic-runner executor):
#   EPIC_RUNNER_STORY_FILE       — path to the story JSON file
#   EPIC_RUNNER_STORY_CODE       — story code (e.g., "ER-042")
#   EPIC_RUNNER_CHANGED_FILES    — path to the changed files tracking file
#   EPIC_RUNNER_STORIES_CACHE    — path to cached story UUIDs file
#   EPIC_RUNNER_MAX_STOP_ITERATIONS — max blocked attempts before allowing stop (default: 3)
#
# Exit codes:
#   0 — allow stop
#   non-zero — block stop (stderr message returned to Claude as instructions)
set -euo pipefail

# ── Helper: commit tracked changed files ──────────────────────────────────
commit_changes() {
    local MSG="$1"
    local CHANGED_FILE="${EPIC_RUNNER_CHANGED_FILES:-}"
    if [ -z "$CHANGED_FILE" ] || [ ! -f "$CHANGED_FILE" ]; then
        return 0
    fi

    # Navigate to repo root
    local REPO_DIR
    REPO_DIR=$(dirname "$(dirname "$(dirname "$STORY_FILE")")")
    cd "$REPO_DIR" 2>/dev/null || return 0

    # Stage all tracked changed files
    local STAGED=0
    while IFS= read -r f; do
        if [ -n "$f" ] && [ -f "$f" ]; then
            git add "$f" 2>/dev/null && STAGED=$((STAGED + 1)) || true
        fi
    done < "$CHANGED_FILE"

    # Commit if there are staged changes
    if [ "$STAGED" -gt 0 ] && ! git diff --cached --quiet 2>/dev/null; then
        git commit -m "$MSG

Tasks completed: ${DONE:-?}/${TOTAL:-?}
Changed files: $STAGED

Co-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>" 2>/dev/null || true
    fi
}

# ── Main logic ────────────────────────────────────────────────────────────

# Read Claude Code hook input from stdin (contains session context)
INPUT=$(cat)

# 1. Extract session ID from hook input
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null || true)
if [ -z "$SESSION_ID" ]; then
    exit 0
fi

# 2. Check if this session ID is a story session (fast local lookup)
STORIES_CACHE="${EPIC_RUNNER_STORIES_CACHE:-}"
if [ -z "$STORIES_CACHE" ] || [ ! -f "$STORIES_CACHE" ]; then
    exit 0
fi

if ! grep -qxF "$SESSION_ID" "$STORIES_CACHE" 2>/dev/null; then
    exit 0
fi

# 3. This IS a story session — load story state
STORY_FILE="${EPIC_RUNNER_STORY_FILE:-}"
STORY_CODE="${EPIC_RUNNER_STORY_CODE:-unknown}"
MAX_STOP_ITERATIONS="${EPIC_RUNNER_MAX_STOP_ITERATIONS:-3}"

if [ -z "$STORY_FILE" ] || [ ! -f "$STORY_FILE" ]; then
    exit 0
fi

# Iteration counter: file-based, incremented each time the hook fires
STOP_COUNT_FILE="${STORY_FILE%.json}.stop_count"
STOP_COUNT=0
if [ -f "$STOP_COUNT_FILE" ]; then
    STOP_COUNT=$(cat "$STOP_COUNT_FILE" 2>/dev/null || echo "0")
fi
STOP_COUNT=$((STOP_COUNT + 1))
echo "$STOP_COUNT" > "$STOP_COUNT_FILE"

# Safety valve: max iterations prevents infinite loops
if [ "$STOP_COUNT" -gt "$MAX_STOP_ITERATIONS" ]; then
    echo "⚠ Max stop iterations ($MAX_STOP_ITERATIONS) exceeded for $STORY_CODE — allowing stop" >&2
    commit_changes "wip($STORY_CODE): max stop iterations exceeded"
    exit 0
fi

# 4. Check blocked status
STATUS=$(jq -r '.status // "in_progress"' "$STORY_FILE" 2>/dev/null || echo "in_progress")
BLOCKED_REASON=$(jq -r '.blocked_reason // ""' "$STORY_FILE" 2>/dev/null || echo "")

if [ "$STATUS" = "blocked" ] && [ -n "$BLOCKED_REASON" ]; then
    echo "Story $STORY_CODE is blocked: $BLOCKED_REASON" >&2
    commit_changes "wip($STORY_CODE): blocked — $BLOCKED_REASON"
    rm -f "$STOP_COUNT_FILE"
    exit 0
fi

# 5. Check task and AC completion
TASKS=$(jq -r '.tasks // []' "$STORY_FILE" 2>/dev/null || echo "[]")
TOTAL=$(echo "$TASKS" | jq 'length')
DONE=$(echo "$TASKS" | jq '[.[] | select(.done == true)] | length')

ACS=$(jq -r '.acceptance_criteria // []' "$STORY_FILE" 2>/dev/null || echo "[]")
TOTAL_ACS=$(echo "$ACS" | jq 'length')

# Block if BOTH tasks AND ACs are empty — story was never groomed
if [ "$TOTAL" -eq 0 ] && [ "$TOTAL_ACS" -eq 0 ]; then
    echo "" >&2
    echo "═══ STOP BLOCKED ($STOP_COUNT/$MAX_STOP_ITERATIONS): Story $STORY_CODE — ungroomed ═══" >&2
    echo "" >&2
    echo "This story has ZERO tasks and ZERO acceptance criteria." >&2
    echo "You must self-groom before completing:" >&2
    echo "  1. Generate at least 2 tasks from the story description" >&2
    echo "  2. Generate at least 1 acceptance criterion (Given/When/Then)" >&2
    echo "  3. Mark tasks done and ACs verified using the CLI commands below" >&2
    echo "" >&2
    echo "═══════════════════════════════════════" >&2
    exit 1
fi

# Allow stop if no tasks defined (partial grooming — has ACs but no tasks)
if [ "$TOTAL" -eq 0 ]; then
    commit_changes "feat($STORY_CODE): completed"
    rm -f "$STOP_COUNT_FILE"
    exit 0
fi

# 6. Block if tasks incomplete — give CLI instructions
if [ "$DONE" -lt "$TOTAL" ]; then
    REMAINING=$((TOTAL - DONE))

    echo "" >&2
    echo "═══ STOP BLOCKED ($STOP_COUNT/$MAX_STOP_ITERATIONS): Story $STORY_CODE ═══" >&2
    echo "" >&2
    echo "$DONE of $TOTAL tasks completed. $REMAINING remaining:" >&2

    # List incomplete tasks with their indices
    for i in $(seq 0 $((TOTAL - 1))); do
        IS_DONE=$(echo "$TASKS" | jq -r ".[$i].done // false")
        if [ "$IS_DONE" != "true" ]; then
            DESC=$(echo "$TASKS" | jq -r ".[$i].description // \"(unnamed)\"")
            echo "  [$i] $DESC" >&2
        fi
    done

    echo "" >&2
    echo "To mark tasks as done, run these commands:" >&2
    for i in $(seq 0 $((TOTAL - 1))); do
        IS_DONE=$(echo "$TASKS" | jq -r ".[$i].done // false")
        if [ "$IS_DONE" != "true" ]; then
            echo "  epic-runner backlog task-done $STORY_CODE $i" >&2
        fi
    done

    # Show AC verification commands if any are unverified
    UNVERIFIED_ACS=0
    for i in $(seq 0 $((TOTAL_ACS - 1))); do
        IS_VERIFIED=$(echo "$ACS" | jq -r ".[$i].verified // false")
        if [ "$IS_VERIFIED" != "true" ]; then
            UNVERIFIED_ACS=$((UNVERIFIED_ACS + 1))
        fi
    done
    if [ "$UNVERIFIED_ACS" -gt 0 ]; then
        echo "" >&2
        echo "To verify acceptance criteria:" >&2
        for i in $(seq 0 $((TOTAL_ACS - 1))); do
            IS_VERIFIED=$(echo "$ACS" | jq -r ".[$i].verified // false")
            if [ "$IS_VERIFIED" != "true" ]; then
                AC_TEXT=$(echo "$ACS" | jq -r ".[$i].criterion // .[$i].title // \"(unnamed)\"")
                echo "  epic-runner backlog ac-verify $STORY_CODE $i  # $AC_TEXT" >&2
            fi
        done
    fi

    echo "" >&2
    echo "Or if you are blocked, run:" >&2
    echo "  epic-runner backlog block $STORY_CODE --reason \"description of what's blocking you\"" >&2
    echo "" >&2
    echo "You CANNOT stop until tasks are done or story is blocked." >&2
    echo "═══════════════════════════════════════" >&2
    exit 1
fi

# 7. All tasks done — commit and allow stop
commit_changes "feat($STORY_CODE): $(jq -r '.title // "untitled"' "$STORY_FILE" 2>/dev/null)"
rm -f "$STOP_COUNT_FILE"
exit 0
