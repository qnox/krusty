#!/usr/bin/env bash
# Target-directory hygiene for the harness. Sourced by run-tests.sh before the first cargo build.
#
# Cargo never garbage-collects `target/` on stable: every distinct (crate, features, profile, deps)
# hash keeps its artifacts and incremental cache forever, and a rustc that is killed mid-codegen
# (timeout, OOM, Ctrl-C, agent deadline) leaves its per-codegen-unit `*.rcgu.o` temporaries in
# `deps/`. One worktree accumulated 26 GB of such objects in a week. Two guards:
#
# * `gate` is a Cargo *profile* (`--profile gate`), never a target dir. `--target-dir target/gate`
#   or `CARGO_TARGET_DIR=target/gate` builds the *dev* profile into `target/gate/debug`, where
#   nothing reuses or cleans it. Reject that shape, and remove nested profile dirs when found.
# * Prune orphan `*.rcgu.o` older than a cutoff. rustc deletes its object temporaries itself after
#   a successful link, so any that survive past the cutoff belong to a build that died. Fresh ones
#   may still be owned by a live rustc (a sibling shell in the same worktree) and are left alone.

target_hygiene_reject_profile_dir() {
  local root="${1%/}"
  case "$(basename "$root")" in
    gate|gate-*)
      echo "run-tests.sh: refusing CARGO_TARGET_DIR/--target-dir '$1'." >&2
      echo "  'gate' is a Cargo profile, not a target directory: build with '--profile gate'" >&2
      echo "  (run-tests.sh already does). A profile-named target dir nests a dev-profile build" >&2
      echo "  under '$1/debug' that nothing reuses or cleans." >&2
      return 2
      ;;
  esac
}

# target_hygiene_prune <target_root> [max_age_minutes]
target_hygiene_prune() {
  local root="${1%/}" max_age="${2:-360}" profile nested pruned=0 n
  [ -d "$root" ] || return 0
  for nested in "$root"/gate/debug "$root"/gate/release "$root"/gate-*/debug; do
    [ -d "$nested" ] || continue
    echo "run-tests.sh: removing $nested ($(du -sh "$nested" 2>/dev/null | cut -f1)): a nested dev-profile build left by '--target-dir ${nested%/*}'; use '--profile gate'." >&2
    rm -rf "$nested"
  done
  for profile in "$root"/*/deps; do
    [ -d "$profile" ] || continue
    n=$(find "$profile" -maxdepth 1 -name '*.rcgu.o' -type f -mmin "+$max_age" -print -delete 2>/dev/null | wc -l | tr -d ' ' || true)
    pruned=$((pruned + n))
  done
  [ "$pruned" -gt 0 ] && echo "run-tests.sh: pruned $pruned orphan codegen object(s) (*.rcgu.o older than ${max_age}m) under $root" >&2
  return 0
}
