#!/bin/bash
# Stop hook — enforces task completion before allowing session to end.
#
# The epic-runner writes the story JSON to $EPIC_RUNNER_STORY_FILE before launch.
# This hook reads the story's tasks and checks if the builder has marked all
# tasks as done. If not, it blocks the stop and tells Claude what remains.
#
# Environment variables (set by epic-runner executor):
#   EPIC_RUNNER_STORY_FILE — path to the story JSON file
#   EPIC_RUNNER_STORY_CODE — story code (e.g., "ER-042") for logging
#
# Exit codes:
#   0 — allow stop (all tasks done, or story flagged blocked)
#   non-zero — block stop (incomplete tasks, stderr message sent to Claude)
set -euo pipefail

# Read Claude Code hook input from stdin
INPUT=$(cat)

# If no story file configured, allow stop (non-per-story mode)
STORY_FILE="${EPIC_RUNNER_STORY_FILE:-}"
if [ -z "$STORY_FILE" ] || [ ! -f "$STORY_FILE" ]; then
    exit 0
fi

STORY_CODE="${EPIC_RUNNER_STORY_CODE:-unknown}"

# Read the transcript to find the last assistant message containing builder output.
# The hook receives the conversation state — check if structured JSON was emitted.
# We look for the builder output markers in the hook input.
HAS_STORIES=$(echo "$INPUT" | jq -r 'try (.transcript // [])[-1].content // "" | test("\"stories\"") // false' 2>/dev/null || echo "false")

# Check the story file for task completion state
TASKS=$(jq -r '.tasks // []' "$STORY_FILE" 2>/dev/null || echo "[]")
TOTAL=$(echo "$TASKS" | jq 'length')
DONE=$(echo "$TASKS" | jq '[.[] | select(.done == true)] | length')

# Check if story is flagged as blocked (blocked_reason is set)
STATUS=$(jq -r '.status // "in_progress"' "$STORY_FILE" 2>/dev/null || echo "in_progress")
BLOCKED_REASON=$(jq -r '.blocked_reason // ""' "$STORY_FILE" 2>/dev/null || echo "")

# Allow stop if story is blocked with a reason
if [ "$STATUS" = "blocked" ] && [ -n "$BLOCKED_REASON" ]; then
    exit 0
fi

# Allow stop if all tasks are done
if [ "$TOTAL" -gt 0 ] && [ "$DONE" -eq "$TOTAL" ]; then
    exit 0
fi

# Allow stop if there are no tasks (story has no task breakdown yet)
if [ "$TOTAL" -eq 0 ]; then
    exit 0
fi

# Block stop — tell Claude what's incomplete
REMAINING=$((TOTAL - DONE))
INCOMPLETE=$(echo "$TASKS" | jq -r '[.[] | select(.done != true) | .description] | join(", ")')

echo "STOP BLOCKED: Story $STORY_CODE has $REMAINING/$TOTAL tasks incomplete." >&2
echo "Incomplete tasks: $INCOMPLETE" >&2
echo "" >&2
echo "You must either:" >&2
echo "  1. Complete all remaining tasks and mark them done in your output" >&2
echo "  2. Flag the story as blocked with a clear blocked_reason" >&2
echo "" >&2
echo "Do NOT stop until all tasks are done or the story is explicitly blocked." >&2
exit 1
