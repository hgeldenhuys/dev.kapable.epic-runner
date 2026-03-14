#!/bin/bash
# Stop hook — enforces task/AC completion before allowing a story session to end.
#
# Two modes:
#   A. Executor mode (launched by epic-runner sprint-run):
#      ENV vars set: EPIC_RUNNER_STORIES_CACHE, EPIC_RUNNER_STORY_FILE, etc.
#      Fast local check against cached session→story mapping.
#
#   B. Manual mode (launched with: claude --session-id <story-uuid>):
#      No ENV vars. Falls back to API lookup: is session_id a story UUID?
#      Reads .epic-runner/config.toml for API credentials.
#
# Exit codes:
#   0 — allow stop (session ends)
#   2 — BLOCK stop (session continues, stderr fed back to Claude as instructions)
#   1 or other — non-blocking error (Claude shows the message but stops anyway)
#
# IMPORTANT: No `set -e` — accidental non-zero exits must not block the stop.
# Use exit 2 intentionally to block, exit 0 to allow.

# ── Helper: find .epic-runner/config.toml by walking up from CWD ─────────
find_config() {
    local DIR="$PWD"
    while [ "$DIR" != "/" ]; do
        if [ -f "$DIR/.epic-runner/config.toml" ]; then
            echo "$DIR/.epic-runner/config.toml"
            return 0
        fi
        DIR=$(dirname "$DIR")
    done
    return 1
}

# ── Helper: extract a TOML value (simple key = "value" patterns) ─────────
toml_get() {
    local FILE="$1" KEY="$2"
    grep "^${KEY} " "$FILE" 2>/dev/null | sed 's/.*= *"\(.*\)"/\1/' | head -1
}

# ── Helper: fetch story from API and write to temp file ──────────────────
fetch_story_from_api() {
    local SESSION_ID="$1"
    local CONFIG
    CONFIG=$(find_config) || return 1

    local BASE_URL DATA_KEY
    BASE_URL=$(toml_get "$CONFIG" "base_url") || return 1
    DATA_KEY=$(toml_get "$CONFIG" "data_key") || return 1

    if [ -z "$BASE_URL" ] || [ -z "$DATA_KEY" ]; then
        return 1
    fi

    # Try to fetch this UUID as a story
    local RESPONSE
    RESPONSE=$(curl -sf --max-time 5 \
        -H "x-api-key: $DATA_KEY" \
        "${BASE_URL}/v1/stories/${SESSION_ID}" 2>/dev/null) || return 1

    # Verify it's valid JSON with an id field
    local STORY_ID
    STORY_ID=$(echo "$RESPONSE" | jq -r '.id // empty' 2>/dev/null) || return 1
    if [ -z "$STORY_ID" ] || [ "$STORY_ID" != "$SESSION_ID" ]; then
        return 1
    fi

    # Write to temp file for validation
    local TMPFILE="/tmp/epic-runner-story-${SESSION_ID}.json"
    echo "$RESPONSE" > "$TMPFILE"
    echo "$TMPFILE"
    return 0
}

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

# ── Read stdin ────────────────────────────────────────────────────────────
INPUT=$(cat)

SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null || true)
if [ -z "$SESSION_ID" ]; then
    exit 0
fi

# ── Mode A: Executor mode (fast local check) ─────────────────────────────
STORIES_CACHE="${EPIC_RUNNER_STORIES_CACHE:-}"
STORY_FILE="${EPIC_RUNNER_STORY_FILE:-}"
MODE=""

if [ -n "$STORIES_CACHE" ] && [ -f "$STORIES_CACHE" ]; then
    if grep -qxF "$SESSION_ID" "$STORIES_CACHE" 2>/dev/null; then
        # Session ID found in cache — executor mode
        if [ -n "$STORY_FILE" ] && [ -f "$STORY_FILE" ]; then
            MODE="executor"
            STORY_CODE="${EPIC_RUNNER_STORY_CODE:-unknown}"
        fi
    fi
fi

# ── Mode B: Manual mode (API fallback) ───────────────────────────────────
if [ -z "$MODE" ]; then
    TMPFILE=$(fetch_story_from_api "$SESSION_ID") || exit 0
    MODE="manual"
    STORY_FILE="$TMPFILE"
    STORY_CODE=$(jq -r '.code // "unknown"' "$STORY_FILE" 2>/dev/null || echo "unknown")
fi

# ── From here, both modes share the same validation logic ────────────────
MAX_STOP_ITERATIONS="${EPIC_RUNNER_MAX_STOP_ITERATIONS:-3}"

# Iteration counter: file-based, incremented each time the hook fires
STOP_COUNT_FILE="/tmp/epic-runner-stop-${SESSION_ID}.count"
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

# Get story title for context
STORY_TITLE=$(jq -r '.title // "untitled"' "$STORY_FILE" 2>/dev/null || echo "untitled")

# Block if BOTH tasks AND ACs are empty — story was never groomed
if [ "$TOTAL" -eq 0 ] && [ "$TOTAL_ACS" -eq 0 ]; then
    echo "" >&2
    echo "═══ STOP BLOCKED ($STOP_COUNT/$MAX_STOP_ITERATIONS): Story $STORY_CODE — ungroomed ═══" >&2
    echo "" >&2
    echo "Your session ID ($SESSION_ID) is story $STORY_CODE: \"$STORY_TITLE\"" >&2
    echo "You were launched to work on this story. You cannot stop until it's done." >&2
    echo "" >&2
    echo "This story has ZERO tasks and ZERO acceptance criteria." >&2
    echo "You must self-groom before completing:" >&2
    echo "  1. Read the story: epic-runner backlog show $STORY_CODE" >&2
    echo "  2. Implement the work described in the story" >&2
    echo "  3. Mark tasks done and ACs verified using the CLI commands below" >&2
    echo "" >&2
    echo "═══════════════════════════════════════" >&2
    exit 2
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
    echo "Your session ID ($SESSION_ID) is story $STORY_CODE: \"$STORY_TITLE\"" >&2
    echo "You were launched to work on this story. You cannot stop until it's done." >&2
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
    exit 2
fi

# 7. All tasks done — commit and allow stop
commit_changes "feat($STORY_CODE): $(jq -r '.title // "untitled"' "$STORY_FILE" 2>/dev/null)"
rm -f "$STOP_COUNT_FILE"
exit 0
