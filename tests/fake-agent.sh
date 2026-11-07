#!/bin/sh
# Stands in for Claude Code in the end-to-end tests: every line pasted into
# this pane is appended to the file named by $1.
out=$1
while IFS= read -r line; do
    printf '%s\n' "$line" >>"$out"
done
