#!/usr/bin/env bash
# Scans the content about to be pushed (or a given ref range) for secrets and
# personal data. Generic token patterns live here; maintainer-specific patterns
# (usernames, real project names, emails) go in .git/info/leak-patterns, which
# is never committed.
#
# Usage:
#   scripts/leak-check.sh                 # scan all tracked files at HEAD
#   scripts/leak-check.sh <base>..<head>  # scan files changed in a range
# Exit code 1 when anything matches.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

generic='(ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|gho_[A-Za-z0-9]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|sk-ant-[A-Za-z0-9_-]{10,}|sk-[A-Za-z0-9]{32,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----|"refresh_token"[[:space:]]*:[[:space:]]*"[^"<]{8,}|"access_token"[[:space:]]*:[[:space:]]*"[^"<]{8,}|ctx7sk[A-Za-z0-9_-]{8,})'

local_file="$(git rev-parse --git-path info/leak-patterns)"

if [[ $# -gt 0 ]]; then
  files=$(git diff --name-only --diff-filter=ACMR "$1")
else
  files=$(git ls-files)
fi

[[ -z "$files" ]] && { echo "leak-check: taranacak dosya yok"; exit 0; }

found=0
while IFS= read -r f; do
  [[ -f "$f" ]] || continue
  [[ "$f" == "scripts/leak-check.sh" ]] && continue
  if grep -nIE "$generic" -- "$f" >/dev/null 2>&1; then
    echo "leak-check: sır deseni: $f"
    grep -nIoE "$generic" -- "$f" | sed -E 's/(:[^:]{0,12}).*/\1.../'
    found=1
  fi
  if [[ -f "$local_file" ]]; then
    while IFS= read -r pat; do
      [[ -z "$pat" || "$pat" == \#* ]] && continue
      if grep -nIiF -- "$pat" "$f" >/dev/null 2>&1; then
        echo "leak-check: kişisel desen ($pat): $f"
        grep -nIiF -- "$pat" "$f" | cut -d: -f1 | head -5 | sed 's/^/  satır /'
        found=1
      fi
    done < "$local_file"
  fi
done <<< "$files"

if [[ $found -eq 1 ]]; then
  echo "leak-check: BAŞARISIZ"
  exit 1
fi
echo "leak-check: temiz"
