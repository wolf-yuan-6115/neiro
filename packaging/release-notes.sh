#!/usr/bin/env bash
# Release notes for v<version>, from the Conventional Commit subjects since the
# previous tag: features, fixes and performance first, then everything else.
# Used by the release CI job; run it locally to preview:
#   packaging/release-notes.sh 0.1.0 HEAD
# Usage: release-notes.sh <version> [<ref>] (ref defaults to the tag v<version>).
set -euo pipefail

version=$1
ref=${2:-v$version}
project_url=${CI_PROJECT_URL:-https://gitlab.com/wolf-yuan/neiro}

prev=$(git describe --tags --abbrev=0 --match 'v[0-9]*' "$ref^" 2>/dev/null || true)
range=${prev:+$prev..}$ref

# "type(scope)!: subject" -> "- **scope:** subject", for one commit type.
section() {
	local title=$1 type=$2 lines
	lines=$(git log --no-merges --reverse --format=%s "$range" |
		sed -nE "s/^${type}(\(([^)]*)\))?(!)?: (.*)$/- \3**\2:** \4/p" |
		sed -E 's/^- (!?)\*\*:\*\* /- \1/; s/^- !/- **Breaking:** /')
	if [ -n "$lines" ]; then
		printf '### %s\n\n%s\n\n' "$title" "$lines"
	fi
}

section Features feat
section Fixes fix
section Performance perf

# Everything else (docs, ci, chore, refactor, …) keeps its type as the label.
others=$(git log --no-merges --reverse --format=%s "$range" |
	grep -vE '^(feat|fix|perf)(\([^)]*\))?!?: ' |
	sed -E 's/^([a-z]+)(\([^)]*\))?(!)?: (.*)$/- \3**\1\2:** \4/; t; s/^/- /' |
	sed -E 's/^- !/- **Breaking:** /' || true)
if [ -n "$others" ]; then
	printf '### Other changes\n\n%s\n\n' "$others"
fi

if [ -n "$prev" ]; then
	printf 'Full changes: %s/-/compare/%s...%s\n\n' "$project_url" "$prev" "v$version"
fi

cat <<EOF
### Downloads

- **Prebuilt binary (x86_64, glibc 2.36 or newer):** \`neiro-$version-x86_64-linux-gnu.tar.gz\`
- **Debian / Ubuntu:** \`neiro_$version-1_amd64.deb\`
- **Fedora / openSUSE:** \`neiro-$version-1.x86_64.rpm\`
EOF
