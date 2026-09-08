#!/bin/sh
# Print one version's section of CHANGELOG.md, for use as the GitHub release body.
#
# The release page showed the same fixed install blurb on every version while
# the CHANGELOG carried the actual notes, so a reader on GitHub could not tell
# one release from the next. This is what closes that gap.
#
# Usage: release-notes.sh <version> [changelog]
# Accepts both heading styles in use across these repos: "## 1.2.3" and
# "## [1.2.3] - DATE". Prints nothing when the version has no section, so a
# caller can tell "no notes" from "notes that happen to be short".
set -eu

version=${1:?usage: release-notes.sh <version> [changelog]}
version=${version#v}
changelog=${2:-CHANGELOG.md}

awk -v v="$version" '
  substr($0, 1, 3) == "## " {
    # The bare version token, whichever heading style this file uses.
    t = substr($0, 4)
    sub(/^\[/, "", t)
    sub(/\].*$/, "", t)
    sub(/[ \t].*$/, "", t)
    if (on) exit
    if (t == v) on = 1
    next
  }
  on { buf[n++] = $0 }
  END {
    s = 0
    while (s < n && buf[s] ~ /^[ \t]*$/) s++
    e = n - 1
    while (e >= s && buf[e] ~ /^[ \t]*$/) e--
    for (i = s; i <= e; i++) print buf[i]
  }
' "$changelog"
