#!/usr/bin/env bash
# Shared helpers for runners that split one libtest binary without splitting top-level modules.

libtest_require_positive_shard_count() {
  local shards="$1" owner="$2"
  case "$shards" in
    '' | *[!0-9]* | 0*)
      echo "$owner: shard count must be a canonical positive integer" >&2
      return 2
      ;;
  esac
}

libtest_write_shard_plan() {
  local binary="$1" shards="$2" listing="$3" plan="$4" seconds="$5"
  run_with_deadline "$seconds" "$binary" --list --format terse >"$listing"
  scripts/libtest-shard-plan.sh "$shards" <"$listing" >"$plan"
}

libtest_shard_expected_tests() {
  local plan="$1" shard="$2"
  awk -F '\t' -v shard="$shard" '$1 == shard { n += $2 } END { print n + 0 }' "$plan"
}

# Emit portable substring skips for every module outside one shard. A top-level `module::` pattern
# normally excludes the whole module with one argument. If that prefix also appears inside an
# included test name (for example `annotations::` inside `type_annotations::...`), fall back to the
# excluded module's full test names. Refuse an inherently ambiguous full-name substring rather than
# silently removing an included test; the runner's selected-set assertion remains a second guard.
libtest_shard_skip_patterns() {
  local plan="$1" listing="$2" shard="$3"
  awk -F '\t' -v shard="$shard" '
    NR == FNR { assigned[$3] = $1; next }
    /: test$/ {
      name = $0
      sub(/: test$/, "", name)
      split(name, path, "::")
      count++
      names[count] = name
      modules[count] = path[1]
      seen[path[1]] = 1
    }
    END {
      for (module in seen) {
        if (assigned[module] == shard) continue
        prefix = module "::"
        safe = 1
        for (i = 1; i <= count; i++) {
          if (assigned[modules[i]] == shard && index(names[i], prefix) != 0) {
            safe = 0
            break
          }
        }
        if (safe) {
          print prefix
          continue
        }
        for (i = 1; i <= count; i++) {
          if (modules[i] != module) continue
          for (j = 1; j <= count; j++) {
            if (assigned[modules[j]] == shard && index(names[j], names[i]) != 0) {
              printf "libtest shard: cannot safely exclude %s; it is a substring of included %s\n", names[i], names[j] > "/dev/stderr"
              invalid = 1
            }
          }
          print names[i]
        }
      }
      if (invalid) exit 3
    }
  ' "$plan" "$listing" | LC_ALL=C sort -u
  local pipeline_status=("${PIPESTATUS[@]}")
  [ "${pipeline_status[0]}" -eq 0 ] && [ "${pipeline_status[1]}" -eq 0 ]
}

libtest_selected_tests() {
  local log="$1"
  awk '
    /^test result: / {
      selected = 0
      for (i = 1; i <= NF; i++) {
        if ($i == "passed;" || $i == "failed;" || $i == "ignored;" || $i == "measured;") {
          selected += $(i - 1)
        }
      }
      seen = 1
    }
    END { if (!seen) exit 2; print selected }
  ' "$log"
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
