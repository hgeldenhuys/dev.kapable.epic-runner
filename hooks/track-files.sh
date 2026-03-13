#!/bin/bash
# PostToolUse hook — tracks files modified by Edit/Write tools.
#
# Appends each modified file path to $EPIC_RUNNER_CHANGED_FILES (one per line).
# The epic-runner reads this file after session end to populate changed_files.
#
# Environment variables:
#   EPIC_RUNNER_CHANGED_FILES — path to the changed files tracking file
set -euo pipefail

CHANGED_FILE="${EPIC_RUNNER_CHANGED_FILES:-}"
if [ -z "$CHANGED_FILE" ]; then
    exit 0
fi

INPUT=$(cat)

# Extract the file path from the tool input
# Edit tool: file_path field; Write tool: file_path field
FILE_PATH=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty' 2>/dev/null || true)

if [ -n "$FILE_PATH" ]; then
    # Append if not already tracked (dedup)
    touch "$CHANGED_FILE"
    if ! grep -qxF "$FILE_PATH" "$CHANGED_FILE" 2>/dev/null; then
        echo "$FILE_PATH" >> "$CHANGED_FILE"
    fi
fi

exit 0
