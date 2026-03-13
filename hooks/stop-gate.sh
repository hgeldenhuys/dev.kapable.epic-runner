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
#   5. If validation fails → block stop with clear instructions (exit 1)
#   6. If validation passes → generate git commit for the work done (exit 0)
#
# Environment variables (set by epic-runner executor):
#   EPIC_RUNNER_STORY_FILE       — path to the story JSON file
#   EPIC_RUNNER_STORY_CODE       — story code (e.g., "ER-042")
#   EPIC_RUNNER_CHANGED_FILES    — path to the changed files tracking file
#   EPIC_RUNNER_STORIES_CACHE    — path to cached story UUIDs file
#
# Exit codes:
#   0 — allow stop
#   non-zero — block stop (stderr message returned to Claude as instructions)
set -euo pipefail

# Read Claude Code hook input from stdin (contains session context)
INPUT=$(cat)

# 1. Extract session ID from hook input
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty' 2>/dev/null || true)
if [ -z "$SESSION_ID" ]; then
    # No session ID available — allow stop
    exit 0
fi

# 2. Check if this session ID is a story session (fast local lookup)
STORIES_CACHE="${EPIC_RUNNER_STORIES_CACHE:-}"
if [ -z "$STORIES_CACHE" ] || [ ! -f "$STORIES_CACHE" ]; then
    # No cache file — not in per-story mode, allow stop
    exit 0
fi

# Check if our session ID is in the cached story UUIDs
if ! grep -qxF "$SESSION_ID" "$STORIES_CACHE" 2>/dev/null; then
    # Not a story session — allow stop
    exit 0
fi

# 3. This IS a story session — validate task/AC completion
STORY_FILE="${EPIC_RUNNER_STORY_FILE:-}"
STORY_CODE="${EPIC_RUNNER_STORY_CODE:-unknown}"

if [ -z "$STORY_FILE" ] || [ ! -f "$STORY_FILE" ]; then
    # Story file missing — allow stop (can't validate without it)
    exit 0
fi

# Check if story is flagged as blocked with a reason
STATUS=$(jq -r '.status // "in_progress"' "$STORY_FILE" 2>/dev/null || echo "in_progress")
BLOCKED_REASON=$(jq -r '.blocked_reason // ""' "$STORY_FILE" 2>/dev/null || echo "")

if [ "$STATUS" = "blocked" ] && [ -n "$BLOCKED_REASON" ]; then
    # Story is blocked with a reason — allow stop, commit the blocked state
    echo "Story $STORY_CODE is blocked: $BLOCKED_REASON" >&2

    # Generate git commit for any work done before blocking
    CHANGED_FILE="${EPIC_RUNNER_CHANGED_FILES:-}"
    if [ -n "$CHANGED_FILE" ] && [ -f "$CHANGED_FILE" ]; then
        REPO_DIR=$(dirname "$(dirname "$STORY_FILE")")
        cd "$REPO_DIR" 2>/dev/null || true
        while IFS= read -r f; do
            [ -n "$f" ] && git add "$f" 2>/dev/null || true
        done < "$CHANGED_FILE"
        if ! git diff --cached --quiet 2>/dev/null; then
            git commit -m "wip($STORY_CODE): blocked — $BLOCKED_REASON" 2>/dev/null || true
        fi
    fi
    exit 0
fi

# Check task completion
TASKS=$(jq -r '.tasks // []' "$STORY_FILE" 2>/dev/null || echo "[]")
TOTAL=$(echo "$TASKS" | jq 'length')
DONE=$(echo "$TASKS" | jq '[.[] | select(.done == true)] | length')

# Allow stop if no tasks defined (story has no task breakdown)
if [ "$TOTAL" -eq 0 ]; then
    exit 0
fi

# 4. Block stop if tasks are incomplete
if [ "$DONE" -lt "$TOTAL" ]; then
    REMAINING=$((TOTAL - DONE))
    INCOMPLETE=$(echo "$TASKS" | jq -r '[.[] | select(.done != true) | .description] | join("\n  - ")')

    echo "" >&2
    echo "═══ STOP BLOCKED: Story $STORY_CODE ═══" >&2
    echo "" >&2
    echo "$DONE of $TOTAL tasks completed. $REMAINING remaining:" >&2
    echo "  - $INCOMPLETE" >&2
    echo "" >&2
    echo "You have two options:" >&2
    echo "  1. COMPLETE the remaining tasks, then mark them done in your structured JSON output" >&2
    echo "  2. FLAG the story as blocked — set status to 'blocked' and provide a blocked_reason" >&2
    echo "     explaining what external dependency or information you need" >&2
    echo "" >&2
    echo "You CANNOT stop until one of these conditions is met." >&2
    echo "═══════════════════════════════════════" >&2
    exit 1
fi

# 5. All tasks done — generate git commit for the completed work
CHANGED_FILE="${EPIC_RUNNER_CHANGED_FILES:-}"
if [ -n "$CHANGED_FILE" ] && [ -f "$CHANGED_FILE" ]; then
    # Navigate to repo root (story file is at .epic-runner/stories/{id}.json)
    REPO_DIR=$(dirname "$(dirname "$(dirname "$STORY_FILE")")")
    cd "$REPO_DIR" 2>/dev/null || true

    # Stage all tracked changed files
    STAGED=0
    while IFS= read -r f; do
        if [ -n "$f" ] && [ -f "$f" ]; then
            git add "$f" 2>/dev/null && STAGED=$((STAGED + 1)) || true
        fi
    done < "$CHANGED_FILE"

    # Commit if there are staged changes
    if [ "$STAGED" -gt 0 ] && ! git diff --cached --quiet 2>/dev/null; then
        # Build commit message from story context
        TITLE=$(jq -r '.title // "untitled"' "$STORY_FILE" 2>/dev/null || echo "untitled")
        git commit -m "feat($STORY_CODE): $TITLE

Tasks completed: $DONE/$TOTAL
Changed files: $STAGED

Co-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>" 2>/dev/null || true
    fi
fi

exit 0
