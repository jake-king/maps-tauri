#!/usr/bin/env bash
# Secrets in the repository, by gitleaks with its default rules (.gitleaks.toml): every commit of the
# checked-out history, and the working tree as a commit would take it (tracked files and untracked
# ones git does not ignore), since the gate runs before the commit and a history scan sees only what
# is already committed. Exit 0: nothing found; 1: a finding; 2: gitleaks missing or not the pinned
# version (never read as a pass).
set -euo pipefail
cd "$(dirname "$0")/.."
GITLEAKS=8.30.1
have=$(gitleaks version 2>/dev/null || true)
# Arch's build of gitleaks prints "version is set by build process"; its package knows the version
if [[ ! $have =~ ^[0-9.]+$ ]] && command -v gitleaks >/dev/null && command -v pacman >/dev/null; then
  have=$(pacman -Qo "$(command -v gitleaks)" 2>/dev/null | awk '{print $NF}' | cut -d- -f1)
fi
if [[ $have != "$GITLEAKS" ]]; then
  echo "secrets: gitleaks $GITLEAKS is needed (found: ${have:-none}); tools/ci-setup.sh installs it" >&2
  exit 2
fi
opts=(--no-banner --redact --verbose --config "$PWD/.gitleaks.toml" --log-level warn)
gitleaks git "${opts[@]}" .
mkdir -p logs
tree=$(mktemp -d -p "$PWD/logs" secrets.XXXX)
trap 'rm -rf "${tree:?}"' EXIT
# a deleted file is still listed until the deletion is staged: tar skips what is not there
git ls-files -z -co --exclude-standard |
  tar --null -T - --ignore-failed-read -cf - 2>/dev/null | tar -xf - -C "$tree"
# scanned from inside the copy, so a finding names the file as the repository does
(cd "$tree" && gitleaks dir "${opts[@]}" .)
echo "secrets: history ($(git rev-list --count HEAD) commits) and working tree clean"
