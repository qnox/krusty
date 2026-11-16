#!/usr/bin/env bash
# Order Kotlin reference versions, oldest first. A pre-release precedes the stable release of the
# same numbers: 2.5.0-Beta1 < 2.5.0-Beta2 < 2.5.0-RC < 2.5.0-RC2 < 2.5.0. GNU `sort -V` orders a
# suffix after that stable release, so version selection does not use it.
set -euo pipefail

awk '
function key(version,    dash, numbers, qualifier, parts, major, minor, patch, channel, pre) {
  dash = index(version, "-")
  if (dash > 0) {
    numbers = substr(version, 1, dash - 1)
    qualifier = substr(version, dash + 1)
  } else {
    numbers = version
    qualifier = ""
  }
  split(numbers, parts, ".")
  major = parts[1] + 0
  minor = parts[2] + 0
  patch = parts[3] + 0
  channel = 2
  pre = 0
  if (qualifier ~ /^Beta[1-9][0-9]*$/) {
    channel = 0
    pre = substr(qualifier, 5) + 0
  } else if (qualifier ~ /^RC[1-9][0-9]*$/) {
    channel = 1
    pre = substr(qualifier, 3) + 0
  } else if (qualifier == "RC") {
    channel = 1
    pre = 0
  } else if (qualifier != "") {
    channel = 3
  }
  return sprintf("%05d %05d %05d %d %05d", major, minor, patch, channel, pre)
}
NF { print key($0) "\t" $0 }
' | sort -t "$(printf '\t')" -k1,1 -u | cut -f2-
