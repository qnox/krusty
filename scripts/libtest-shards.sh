#!/usr/bin/env bash
# Shared check for runners that partition one corpus across processes.

libtest_require_positive_shard_count() {
  local shards="$1" owner="$2"
  case "$shards" in
    '' | *[!0-9]* | 0*)
      echo "$owner: shard count must be a canonical positive integer" >&2
      return 2
      ;;
  esac
}
# Milliseconds strictly above this are reported. `KRUSTY_SLOW_TEST_MS` overrides the default.
libtest_slow_test_threshold() {
  local threshold="${1:-${KRUSTY_SLOW_TEST_MS:-200}}"
  case "$threshold" in
    '' | *[!0-9]*)
      echo "slow-test: KRUSTY_SLOW_TEST_MS must be a non-negative integer" >&2
      return 2
      ;;
  esac
  printf '%s\n' "$threshold"
}

# Tests slower than threshold_ms in a libtest log produced with `--report-time`.
# Each line is `<milliseconds>\t<test name>`, slowest first. A time equal to the threshold is
# omitted. Ignored tests have no duration. POSIX awk: macOS has no gawk match groups.
libtest_slow_tests() {
  local log="$1"
  local threshold
  threshold="$(libtest_slow_test_threshold "${2:-}")" || return $?
  awk -v threshold="$threshold" '
    function to_ms(spec, parts, frac) {
      split(spec, parts, ".")
      frac = parts[2] "000"
      return parts[1] * 1000 + substr(frac, 1, 3)
    }
    index($0, "test ") == 1 {
      rest = substr($0, 6)
      pos = index(rest, " ... ")
      if (pos == 0) next
      name = substr(rest, 1, pos - 1)
      tail = substr(rest, pos + 5)
      if (tail !~ /^(ok|FAILED) <[0-9]+\.[0-9]+s>$/) next
      spec = tail
      sub(/^.*</, "", spec)
      sub(/s>$/, "", spec)
      ms = to_ms(spec)
      if (ms > threshold + 0) printf "%d\t%s\n", ms, name
    }
  ' "$log" | LC_ALL=C sort -t $'\t' -k1,1nr -k2,2
}

# Print greppable lines for every record `<ms>\t<label>\t<name>` in the file, slowest first.
libtest_print_slow_records() {
  local threshold="$1" combined="$2" count
  if [ ! -s "$combined" ]; then
    echo "slow-test: none over ${threshold}ms"
    return 0
  fi
  count="$(awk 'END { print NR + 0 }' "$combined")"
  echo "slow-test: summary count=${count} threshold=${threshold}ms"
  LC_ALL=C sort -t $'\t' -k1,1nr -k3,3 "$combined" | while IFS=$'\t' read -r ms label name; do
    printf 'slow-test: %sms bin=%s test=%s\n' "$ms" "$label" "$name"
  done
}

# Print tests slower than `KRUSTY_SLOW_TEST_MS` (default 200) from libtest `--report-time` logs.
# The label is the log filename without `.log`, so a shard log stays distinct from its binary.
libtest_print_slow_tests() {
  local threshold combined log label
  threshold="$(libtest_slow_test_threshold)" || return $?
  combined="$(mktemp)"
  for log in "$@"; do
    [ -f "$log" ] || continue
    label="$(basename "$log")"
    label="${label%.log}"
    libtest_slow_tests "$log" "$threshold" | while IFS=$'\t' read -r ms name; do
      printf '%s\t%s\t%s\n' "$ms" "$label" "$name"
    done >>"$combined"
  done
  libtest_print_slow_records "$threshold" "$combined"
  local status=$?
  rm -f "$combined"
  return "$status"
}

export -f libtest_slow_test_threshold libtest_slow_tests libtest_print_slow_records libtest_print_slow_tests
