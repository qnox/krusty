# krusty build-system entrypoint.
#
# `just` is the single command CI and contributors share — `just ci` is lint and coverage, and the
# release pipeline reuses these recipes, so anything CI does can be reproduced locally.
#
#   just            list recipes
#   just ci         PR gate: lint + coverage
#   just lint       fmt-check + clippy-baseline-check (fails on NEW clippy findings)
#   just fmt        apply rustfmt
#   just clippy-baseline   refreeze the accepted clippy findings (clippy-baseline.tsv)
#   just test       full test suite (optionally `just test -- <args>`)
#   just test-all   suite against every supported Kotlin version, in parallel
#   just kotlinc    download+unpack the reference kotlinc dist; prints bin path
#   just kotlin-native download+unpack the matching Kotlin/Native distribution; prints root path
#   just klib-semantics exercise the common KLIB metadata decoder against that distribution
#   just box-corpus clone+cache the Kotlin codegen/box corpus; prints box dir
#   just conformance       print box-suite conformance "<pct> <passed> <applicable>" (stderr: JVM bytes)
#   just profile-box [filter]  profile compiler-only box cases; writes target/flamegraph.svg
#   just install-hooks    lefthook install
#   just version          krusty release version, e.g. 2.4.20-build.3
#   just max-version      highest supported Kotlin reference version (release base)
#   just build-number V   build number for reference version V (resets per version)
#   just supported-kotlin comma list of supported Kotlin reference versions
#   just kotlin-versions  supported Kotlin reference versions, one per line
#   just matrix-json      JSON array of the supported versions (CI test matrix)
#   just build-release [target]
#   just package <target>

set shell := ["bash", "-uc"]

mod push

manifest := "kotlin-versions"

# List available recipes.
default:
    @just --list

# === PR gate: the one command the ci job runs ===
# Own tests run once under coverage. Conformance is not here: the workflow builds
# that binary once, runs the box suite for every supported Kotlin version, and
# runs every other test in the binary for every supported Kotlin version.
ci: lint coverage-gate

# Lint gate (enforced locally + in CI + by the pre-commit hook): formatting must be clean, and
# clippy must introduce NO new findings beyond the frozen baseline (clippy-baseline.tsv). Existing
# findings are tolerated; any new one fails. Identical behaviour everywhere — it's plain cargo + sh.
lint: fmt-check clippy-baseline-check

# Run the LSP package without coverage instrumentation.
test-lsp:
    cargo test --profile gate -p krusty-lsp --all-targets

# rustfmt must be clean (the repo is fully formatted; `just fmt` fixes any drift).
fmt-check:
    cargo fmt --all --check

# Apply rustfmt across the workspace.
fmt:
    cargo fmt --all

# Emit current clippy findings as a stable, line-number-independent fingerprint set:
#   <count><TAB><file><TAB><message>   (one per file+message, so a new occurrence bumps the count)
clippy-findings:
    #!/usr/bin/env bash
    set -euo pipefail
    # Cached crates do not re-emit diagnostics, so fingerprint from a clean Clippy target. A
    # package-scoped `cargo clean -p` is not sufficient for a restored CI target: Cargo can report
    # `Removed 0 files` and then reuse every cached first-party artifact, producing a false-empty
    # diagnostic set. Keep Clippy in its own target so the clean cannot discard gate-profile tests.
    clippy_target="$PWD/target/clippy-baseline"
    cargo clean --target-dir "$clippy_target" >/dev/null
    output=$(mktemp)
    trap 'rm -f "$output"' EXIT
    if ! CARGO_TARGET_DIR="$clippy_target" cargo clippy --workspace --all-targets --all-features --message-format=short >"$output" 2>&1; then
        cat "$output" >&2
        exit 1
    fi
    sed -nE 's/^([^:]+):[0-9]+:[0-9]+: warning: (.*)$/\1\t\2/p' "$output" \
      | sort | uniq -c | sed -E 's/^ *([0-9]+) /\1\t/' | sort

# Freeze the current clippy findings as the accepted baseline. Run after intentionally fixing (or
# knowingly accepting) findings; commit the updated clippy-baseline.tsv.
clippy-baseline:
    @just clippy-findings > clippy-baseline.tsv
    @echo "wrote clippy-baseline.tsv ($(wc -l < clippy-baseline.tsv) entries)"

# Fail if clippy reports any finding not already frozen in the baseline (new file+message, or a
# higher count for an existing one). This is what blocks NEW issues while tolerating existing ones.
clippy-baseline-check:
    #!/usr/bin/env bash
    set -euo pipefail
    base="clippy-baseline.tsv"
    [ -f "$base" ] || { echo "missing $base — run 'just clippy-baseline'" >&2; exit 1; }
    cur="$(just clippy-findings)"
    fail=0
    while IFS=$'\t' read -r cnt file msg; do
        [ -z "${file:-}" ] && continue
        b=$(awk -F'\t' -v f="$file" -v m="$msg" '$2==f && $3==m {print $1}' "$base")
        b=${b:-0}
        if [ "$cnt" -gt "$b" ]; then
            echo "NEW clippy finding ($cnt > $b allowed) — $file: $msg" >&2
            fail=1
        fi
    done <<< "$cur"
    if [ "$fail" -ne 0 ]; then
        echo "clippy: new findings beyond baseline. Fix them, or 'just clippy-baseline' if intentional." >&2
        exit 1
    fi
    echo "clippy: no new findings beyond baseline"

# Full test suite through the canonical self-provisioning harness.
test *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    ./run-tests.sh {{ARGS}}

# The whole suite MINUS the slow external-corpus suites (kotlin box corpus, serialization/ksp
# conformance) — the same set coverage measures. A quick dev inner-loop check; the full suite runs
# in CI via `just test-all`.
test-fast:
    #!/usr/bin/env bash
    set -euo pipefail
    export KRUSTY_TEST_EXCLUDE="conformance"
    ./run-tests.sh

# Run ONLY the Kotlin box/codegen conformance suite, plain (no coverage instrumentation) — the
# "conformance without coverage" half of the pre-push gate. Provisions kotlinc + corpus via the
# harness. The suite is internally rayon-parallel, so it uses all cores on its own.
conformance-plain:
    just conformance

# Sample compiler CPU time for the box corpus and write an interactive flamegraph. An optional path
# substring focuses one case, for example: `just profile-box ranges/contains/generated/arrayIndices.kt`.
profile-box FILTER="":
    KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1 KRUSTY_BOX_ONLY="{{FILTER}}" ./run-tests.sh --test conformance kotlin_codegen_box_conformance -- --nocapture

# Run the whole external/reference conformance binary without coverage instrumentation. `just ci`
# does not call this. CI builds the binary once and runs both target box suites and every other test
# for every supported version (see .github/workflows/ci.yml). Keep the memory-heavy JVM and Native
# corpus tests isolated, then run every other conformance test in a fresh process. The Native lane
# compiles, links and runs every accepted case once under its dedicated suite-wide deadline
# (`scripts/native-conformance-run.sh`), and each Wasm lane the same way under Node.js
# (`scripts/box-lane-run.sh`). The final independent JVM-backed tests are threaded (capped
# at 4 — each thread can hold a compiler-server/runner JVM) instead of serializing ~40 tests.
conformance-all-plain:
    just conformance
    just native-conformance-run "$(just conformance-bin)" "$(just max-version)"
    just wasm-conformance-run wasm-js "$(just conformance-bin)" "$(just max-version)"
    just wasm-conformance-run wasm-wasi "$(just conformance-bin)" "$(just max-version)"
    ./run-tests.sh --test conformance -- --skip kotlin_codegen_box_conformance --skip kotlin_codegen_box_native_conformance --skip kotlin_codegen_box_wasm --test-threads=$(n=$(nproc 2>/dev/null || sysctl -n hw.ncpu); [ "$n" -gt 4 ] && n=4; echo $n)

# Run all conformance tests for one runtime-selected Kotlin version. The shared Cargo target avoids
# per-version rebuilds; the reference compiler and corpus are provisioned on demand.
conformance-one VERSION:
    #!/usr/bin/env bash
    set -euo pipefail
    v="{{VERSION}}"
    kc_var="KRUSTY_KOTLINC_${v//./_}"
    kc="${!kc_var:-}"
    vendored="$PWD/target/cache/kotlinc/$v/kotlinc/bin/kotlinc"
    [ -z "$kc" ] && [ -x "$vendored" ] && kc="$vendored"
    [ -z "$kc" ] && kc="${KRUSTY_KOTLINC:-$(just kotlinc "$v")}"
    export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
    export KRUSTY_LANGUAGE_VERSION="$v"
    export KRUSTY_KOTLINC="$kc"
    export KRUSTY_KOTLIN_BOX_DIR="${KRUSTY_KOTLIN_BOX_DIR:-$(just box-corpus "$v")}"
    bin="$(just conformance-bin)"
    just conformance-run "$bin" "$v"
    bash scripts/native-conformance-run.sh "$bin"
    bash scripts/box-lane-run.sh wasm-js "$bin"
    bash scripts/box-lane-run.sh wasm-wasi "$bin"
    conf_threads="$(nproc 2>/dev/null || sysctl -n hw.ncpu)"; [ "$conf_threads" -gt 4 ] && conf_threads=4
    ./run-tests.sh --test conformance -- --skip kotlin_codegen_box_conformance --skip kotlin_codegen_box_native_conformance --skip kotlin_codegen_box_wasm --test-threads="$conf_threads"

# Measure test coverage — regions, functions, lines and BRANCHES — via LLVM source-based coverage
# (nightly, `-Zcoverage-options=branch`). Runs an instrumented build + the own suite in parallel and
# writes target/coverage/{full,summary}.json. External corpus/conformance suites are excluded.
coverage:
    scripts/coverage.sh

# Enforce coverage >= the committed master baseline (coverage-baseline.json). Fails if any metric
# regresses. This is the coverage half of the enforcing git-hook gate; run it locally the same way.
coverage-gate:
    scripts/coverage-gate.sh

# Re-measure and overwrite coverage-baseline.json. Run only for an intentional coverage change, and
# commit the refreshed baseline with it.
coverage-bless:
    scripts/coverage-bless.sh

# Perturbation / mutation testing (test-QUALITY, not coverage): mutate the code and check a test
# fails. A SURVIVING mutant is code that no test kills — a coverage gap, or a test that asserts
# nothing. Run occasionally, not per-commit: each mutant rebuilds + reruns the suite, so it is slow.
# With no args it mutates only the diff vs origin/master (recent work — tractable); pass cargo-mutants
# args to scope otherwise, e.g. `just mutants -f src/resolve.rs`. Needs `cargo install cargo-mutants`.
mutants *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    export PATH="$HOME/.cargo/bin:$PATH"
    v="$(just max-version)"
    export KRUSTY_KOTLINC="${KRUSTY_KOTLINC:-$(just kotlinc "$v")}"
    export KRUSTY_KOTLIN_BOX_DIR="${KRUSTY_KOTLIN_BOX_DIR:-$(just box-corpus "$v")}"
    set -- {{ARGS}}
    if [ "$#" -eq 0 ]; then
        mkdir -p target
        base="$(git rev-parse -q --verify origin/master >/dev/null 2>&1 && echo origin/master || echo HEAD~1)"
        git diff "$base"...HEAD > target/mutants.diff
        if [ ! -s target/mutants.diff ]; then
            echo "mutants: no diff vs $base — pass a scope, e.g. 'just mutants -f src/resolve.rs'" >&2
            exit 0
        fi
        set -- --in-diff target/mutants.diff
    fi
    cargo mutants "$@"

# Download + unpack the reference Kotlin compiler distribution into one self-contained dir
# (target/cache/kotlinc/<ver>/), and print the path to its `bin/kotlinc`. Idempotent — a no-op once unpacked
# (so it's cheap to cache). This is the reference toolchain the differential harness validates
# against; its `lib/kotlin-stdlib.jar` is also what the box e2e tests put on the runtime classpath.
kotlinc VERSION=`just max-version`:
    #!/usr/bin/env bash
    set -euo pipefail
    ver="{{VERSION}}"
    dest="$PWD/target/cache/kotlinc/$ver"
    bin="$dest/kotlinc/bin/kotlinc"
    if [ -x "$bin" ]; then echo "$bin"; exit 0; fi
    url="https://github.com/JetBrains/kotlin/releases/download/v${ver}/kotlin-compiler-${ver}.zip"
    tmp="$(mktemp -d)"
    echo "downloading kotlin-compiler ${ver}…" >&2
    curl -fsSL "$url" -o "$tmp/kotlinc.zip" || { echo "failed to download $url" >&2; rm -rf "$tmp"; exit 1; }
    mkdir -p "$dest"
    if command -v unzip >/dev/null 2>&1; then unzip -q "$tmp/kotlinc.zip" -d "$dest"
    elif command -v python3 >/dev/null 2>&1; then python3 -c "import zipfile; zipfile.ZipFile('$tmp/kotlinc.zip').extractall('$dest')"
    else ( cd "$dest" && jar xf "$tmp/kotlinc.zip" ); fi
    rm -rf "$tmp"
    chmod +x "$bin"
    echo "$bin"

# Provision the Kotlin/Native distribution whose stdlib KLIB supplies the real metadata format.
# The extracted root is versioned and cached beside the JVM compiler distribution.
kotlin-native VERSION=`just max-version`:
    @scripts/kotlin-native.sh "{{VERSION}}" "$PWD/target/cache/kotlin-native"

# This lane is deliberately required: the integration test may be optional in an ordinary local
# run, but this recipe provisions the distribution and turns absence or an undecodable fragment
# into a failure.
klib-semantics VERSION=`just max-version`:
    #!/usr/bin/env bash
    set -euo pipefail
    root="$(just kotlin-native "{{VERSION}}")"
    KRUSTY_KOTLIN_NATIVE="$root" KRUSTY_REQUIRE_KLIB=1 \
      ./run-tests.sh --test e2e klib_ -- --nocapture

# Provision the Kotlin codegen/box conformance corpus into one cached dir (target/cache/box-corpus/<ver>/) and
# print the path to compiler/testData/codegen/box. Blobless + sparse clone of just that directory at
# the matching tag — small and idempotent (no-op once present, cheap to cache). Mirrors `kotlinc`:
# the conformance test FAILS (not skips) without it, so the harness provisions it rather than
# silently skipping. The same checkout carries JetBrains' mock JDK,
# which every box test without `// FULL_JDK` compiles against; a cache that predates it gains it
# through `sparse-checkout add` instead of counting as complete.
box-corpus VERSION=`just max-version`:
    #!/usr/bin/env bash
    set -euo pipefail
    ver="{{VERSION}}"
    root="$PWD/target/cache/box-corpus/$ver"
    box="$root/compiler/testData/codegen/box"
    # Git hooks export repository-local GIT_* variables. Without clearing them, `git -C "$root"`
    # still operates on the outer krusty worktree and can replace its sparse-checkout definition.
    while read -r name; do unset "$name"; done < <(git rev-parse --local-env-vars)
    # The mock JDK's directory at this tag (KtTestUtil.findMockJdkRtJar): JetBrains moved it from
    # compiler/testData/mockJDK to third-party/mockJDKs/mockJDK. Exactly one exists per tag; print
    # nothing when the checkout cannot name one. `cat-file -t HEAD:<path>` is stable across git
    # releases, unlike `ls-tree` pathspec matching.
    mock_dir_at_tag() {
        local found=()
        local dir
        for dir in third-party/mockJDKs/mockJDK/jre/lib compiler/testData/mockJDK/jre/lib; do
            if [ "$(git -C "$root" cat-file -t "HEAD:$dir" 2>/dev/null)" = tree ]; then
                found+=("$dir")
            fi
        done
        if [ "${#found[@]}" -eq 1 ]; then printf '%s\n' "${found[0]}"; fi
    }
    clone_corpus() {
        echo "cloning Kotlin codegen/box corpus (v${ver})…" >&2
        rm -rf "$root"
        git clone --depth 1 --filter=blob:none --sparse --branch "v${ver}" \
            https://github.com/JetBrains/kotlin.git "$root" >&2 \
            || { echo "failed to clone JetBrains/kotlin v${ver}" >&2; rm -rf "$root"; exit 1; }
        mock_dir="$(mock_dir_at_tag)"
        [ -n "$mock_dir" ] \
            || { echo "JetBrains/kotlin v${ver} has no single mock JDK directory" >&2; exit 1; }
        # Keep cone mode: a fresh sparse clone checks out the repository-root files that cone mode
        # owns. Switching to non-cone while excluding them can leave those paths in place and abort
        # the update before the requested corpus directory is materialized.
        git -C "$root" sparse-checkout set compiler/testData/codegen/box "$mock_dir" >&2
    }
    mock_dir=""
    if [ -d "$root/.git" ] && [ -d "$box" ]; then
        mock_dir="$(mock_dir_at_tag)"
    fi
    if [ -n "$mock_dir" ] && [ -f "$root/$mock_dir/rt.jar" ]; then
        echo "$box"
        exit 0
    elif [ -n "$mock_dir" ]; then
        # A cache provisioned before the mock JDK was needed: extend it rather than accept it.
        echo "adding the mock JDK to the Kotlin codegen/box corpus (v${ver})…" >&2
        git -C "$root" sparse-checkout add "$mock_dir" >&2 \
            || { echo "failed to add $mock_dir to the v${ver} corpus checkout" >&2; exit 1; }
    else
        # No checkout, or a restored one whose tree cannot name its mock JDK: provision afresh.
        clone_corpus
    fi
    [ -d "$box" ] || { echo "box dir missing after sparse checkout: $box" >&2; exit 1; }
    [ -f "$root/$mock_dir/rt.jar" ] \
        || { echo "mock JDK missing after sparse checkout: $root/$mock_dir/rt.jar" >&2; exit 1; }
    echo "$box"

# Provision the kotlinx.serialization compiler-plugin box corpus (plugins/kotlinx-serialization/
# testData/boxIr) from JetBrains/kotlin at the matching tag — blobless+sparse, idempotent. The
# reference suite our own serialization conformance tests mirror (and must meet-or-exceed). Point
# KRUSTY_SER_BOXIR_DIR at the printed path.
ser-corpus VERSION=`just max-version`:
    #!/usr/bin/env bash
    set -euo pipefail
    ver="{{VERSION}}"
    root="$PWD/target/cache/ser-corpus/$ver"
    box="$root/plugins/kotlinx-serialization/testData/boxIr"
    if [ -d "$box" ]; then echo "$box"; exit 0; fi
    echo "cloning kotlinx.serialization boxIr corpus (v${ver})…" >&2
    rm -rf "$root"
    while read -r name; do unset "$name"; done < <(git rev-parse --local-env-vars)
    git clone --depth 1 --filter=blob:none --sparse --branch "v${ver}" \
        https://github.com/JetBrains/kotlin.git "$root" >&2 \
        || { echo "failed to clone JetBrains/kotlin v${ver}" >&2; rm -rf "$root"; exit 1; }
    git -C "$root" sparse-checkout set plugins/kotlinx-serialization/testData >&2
    [ -d "$box" ] || { echo "serialization boxIr dir missing after sparse checkout: $box" >&2; exit 1; }
    echo "$box"

# Provision the KSP reference test corpus (google/ksp kotlin-analysis-api/testData + its test-utils
# processors) — shallow+sparse, idempotent. The authoritative KSP capability suite our own KSP
# conformance tests mirror (and must meet-or-exceed). Point KRUSTY_KSP_TESTDATA_DIR at the printed
# path. KSP_REF defaults to the repo default branch (no version tags published as branches).
ksp-corpus KSP_REF="main":
    #!/usr/bin/env bash
    set -euo pipefail
    ref="{{KSP_REF}}"
    root="$PWD/target/cache/ksp-corpus/$ref"
    td="$root/kotlin-analysis-api/testData"
    if [ -d "$td" ]; then echo "$td"; exit 0; fi
    echo "cloning google/ksp test corpus (${ref})…" >&2
    rm -rf "$root"
    while read -r name; do unset "$name"; done < <(git rev-parse --local-env-vars)
    git clone --depth 1 --filter=blob:none --sparse --branch "$ref" \
        https://github.com/google/ksp.git "$root" >&2 \
        || { echo "failed to clone google/ksp ${ref}" >&2; rm -rf "$root"; exit 1; }
    git -C "$root" sparse-checkout set kotlin-analysis-api/testData test-utils >&2
    [ -d "$td" ] || { echo "ksp testData dir missing after sparse checkout: $td" >&2; exit 1; }
    echo "$td"

# Run the full suite against EVERY supported Kotlin reference version, in parallel — locally and in
# CI alike. Parallelization lives here, not in a CI matrix, so `just test-all` behaves identically
# everywhere and needs no GitHub-Actions infra. Each version gets its own CARGO_TARGET_DIR (so the
# concurrent cargo runs don't fight over the build lock) and its own log; KRUSTY_LANGUAGE_VERSION is
# exported for the differential harness. The actual suite still goes through `run-tests.sh`, which uses
# the disk-lean `gate` profile instead of Cargo's default debug test profile. Parallel sessions stay
# isolated by the per-version target dirs; do not recycle space by deleting a tree another run may use.
# Incremental state is disabled for these per-version CI-style targets; deps and test binaries are still
# retained and reused, but Cargo skips the largest low-value cache layer.
#
# Set KRUSTY_KOTLINC_<ver> (dots->underscores) to point a version at a specific kotlinc, else the
# vendored dist from `just kotlinc` (.kotlinc/<ver>/...) is used automatically, else the ambient
# KRUSTY_KOTLINC.
test-all *ARGS:
    #!/usr/bin/env bash
    set -uo pipefail
    mkdir -p target
    # Pre-provision sequentially (idempotent) so the parallel runs below don't race on the same
    # clone/download. Each version gets its own kotlinc dist + box corpus.
    while read -r v; do
        [ -z "$v" ] && continue
        just kotlinc "$v" >/dev/null || exit 1
        just box-corpus "$v" >/dev/null || exit 1
    done < <(just kotlin-versions)
    declare -a pids=() tags=()
    while read -r v; do
        [ -z "$v" ] && continue
        kc_var="KRUSTY_KOTLINC_${v//./_}"
        kc="${!kc_var:-}"
        vendored="$PWD/target/cache/kotlinc/$v/kotlinc/bin/kotlinc"
        [ -z "$kc" ] && [ -x "$vendored" ] && kc="$vendored"
        [ -z "$kc" ] && kc="${KRUSTY_KOTLINC:-}"
        ( CARGO_TARGET_DIR="target/kt-$v" \
          CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}" \
          KRUSTY_LANGUAGE_VERSION="$v" \
          KRUSTY_KOTLINC="$kc" \
          KRUSTY_KOTLIN_BOX_DIR="$PWD/target/cache/box-corpus/$v/compiler/testData/codegen/box" \
          ./run-tests.sh {{ARGS}} > "target/test-$v.log" 2>&1 ) &
        pids+=("$!"); tags+=("$v")
    done < <(just kotlin-versions)
    [ "${#pids[@]}" -gt 0 ] || { echo "no Kotlin versions in the manifest" >&2; exit 1; }
    rc=0
    for i in "${!pids[@]}"; do
        if wait "${pids[$i]}"; then
            echo "ok:     Kotlin ${tags[$i]}"
        else
            echo "FAILED: Kotlin ${tags[$i]}  (tail target/test-${tags[$i]}.log)" >&2
            tail -n 15 "target/test-${tags[$i]}.log" >&2
            rc=1
        fi
    done
    exit $rc

# --- Kotlin box-suite conformance (drives the README conformance and JVM byte-equality badges) ---

# Build the krusty-build library test binary and print its path. Gradle lanes run this prebuilt
# binary; they do not compile krusty again.
krusty-build-tests:
    #!/usr/bin/env bash
    set -euo pipefail
    bin=$(cargo test --no-run --profile gate -p krusty-build --lib --message-format=json \
      | jq -r 'select(.reason=="compiler-artifact" and .target.name=="krusty_build" and .executable != null) | .executable // empty' \
      | tail -1)
    [ -n "$bin" ] && [ -x "$bin" ] || { echo "could not locate krusty-build test binary" >&2; exit 1; }
    printf '%s\n' "$bin"

# Build the conformance test binary and print its path.
conformance-bin:
    #!/usr/bin/env bash
    set -euo pipefail
    bin=$(cargo test --no-run --profile gate --test conformance --message-format=json \
      | jq -r 'select(.reason=="compiler-artifact" and .target.name=="conformance") | .executable // empty' \
      | tail -1)
    [ -n "$bin" ] && [ -x "$bin" ] || { echo "could not locate conformance test binary" >&2; exit 1; }
    printf '%s\n' "$bin"

# Run the codegen/box conformance suite and print the case report "<pct> <passed> <applicable>":
# applicable cases whose box() returns "OK". The same run's JVM byte report "<pct> <matched> <total>",
# matching `.class` bytes against the same version's kotlinc over those cases, goes to stderr and,
# when BYTES names a file, to BYTES (see scripts/conformance-report.sh). The suite's exit status
# separately holds every file's box() outcome to the version's exact fail/not-applicable manifests.
conformance VERSION=`just max-version` BYTES="":
    #!/usr/bin/env bash
    set -euo pipefail
    just conformance-run "$(just conformance-bin)" "{{VERSION}}" "{{BYTES}}"

# Run a PREBUILT conformance test binary (path BIN) against Kotlin VERSION and print the case report
# "<pct> <passed> <applicable>" summed over every shard; the JVM byte report "<pct> <matched> <total>"
# summed the same way goes to stderr and to BYTES when given. The test writes both reports before its
# assertions, so callers receive the metrics even when a file disagrees with the outcome manifests.
conformance-run BIN VERSION BYTES="":
    #!/usr/bin/env bash
    set -euo pipefail
    bash scripts/conformance-run.sh "{{BIN}}" "{{VERSION}}" "{{BYTES}}"

# Run the Native codegen/box ratchet with a prebuilt conformance binary. Provisioning is explicit
# here so a fresh CI step gets the exact compiler/corpus selected by VERSION rather than depending
# on environment mutations from an earlier shell.
native-conformance-run BIN VERSION REPORT="":
    #!/usr/bin/env bash
    set -euo pipefail
    v="{{VERSION}}"
    kotlinc="${KRUSTY_KOTLINC:-$(just kotlinc "$v")}"
    box_dir="${KRUSTY_KOTLIN_BOX_DIR:-$(just box-corpus "$v")}"
    KRUSTY_LANGUAGE_VERSION="$v" \
      KRUSTY_KOTLINC="$kotlinc" \
      KRUSTY_KOTLIN_BOX_DIR="$box_dir" \
      bash scripts/native-conformance-run.sh "{{BIN}}" {{ if REPORT != "" { quote(REPORT) } else { "" } }}

# Run one Wasm box lane (`wasm-js` or `wasm-wasi`) of a prebuilt conformance binary for one Kotlin
# version, as `native-conformance-run` does for Native. Needs Node.js 22 or newer on PATH.
wasm-conformance-run LANE BIN VERSION REPORT="":
    #!/usr/bin/env bash
    set -euo pipefail
    v="{{VERSION}}"
    kotlinc="${KRUSTY_KOTLINC:-$(just kotlinc "$v")}"
    box_dir="${KRUSTY_KOTLIN_BOX_DIR:-$(just box-corpus "$v")}"
    KRUSTY_LANGUAGE_VERSION="$v" \
      KRUSTY_KOTLINC="$kotlinc" \
      KRUSTY_KOTLIN_BOX_DIR="$box_dir" \
      bash scripts/box-lane-run.sh "{{LANE}}" "{{BIN}}" {{ if REPORT != "" { quote(REPORT) } else { "" } }}

# Run every non-box conformance test with the binary from `conformance-bin`. CI invokes this beside
# `conformance-run` in every supported-version JVM row. A sibling `krusty` next to BIN is the CLI.
conformance-regressions BIN VERSION:
    #!/usr/bin/env bash
    set -euo pipefail
    bash scripts/conformance-regressions.sh "{{BIN}}" "{{VERSION}}"

# Preview the shields.io endpoint badges in docs/badges/*.json (untracked) for the max version.
# With no arguments it runs both target lanes; pass CASES, BYTES, and NATIVE together to render
# downloaded reports. The master release job publishes the same payloads; nothing here publishes.
conformance-badge CASES="" BYTES="" NATIVE="":
    #!/usr/bin/env bash
    set -euo pipefail
    v="$(just max-version)"
    cases='{{CASES}}'
    bytes='{{BYTES}}'
    native='{{NATIVE}}'
    if [ -z "$cases" ] && [ -z "$bytes" ] && [ -z "$native" ]; then
      mkdir -p target
      cases="target/conformance-$v.report"
      bytes="target/jvm-byte-equality-$v.report"
      native="target/native-conformance-$v.report"
      just conformance "$v" "$bytes" > "$cases"
      just native-conformance-run "$(just conformance-bin)" "$v" "$native"
    elif [ -z "$cases" ] || [ -z "$bytes" ] || [ -z "$native" ]; then
      echo "conformance-badge: pass CASES, BYTES, and NATIVE together, or none" >&2
      exit 2
    fi
    mkdir -p docs/badges
    trap 'rm -f docs/badges/conformance.json.tmp docs/badges/jvm-byte-equality.json.tmp docs/badges/native-conformance.json.tmp' EXIT
    bash scripts/conformance-badge.sh json conformance "$cases" "$v" > docs/badges/conformance.json.tmp
    bash scripts/conformance-badge.sh json jvm-byte-equality "$bytes" "$v" \
      > docs/badges/jvm-byte-equality.json.tmp
    bash scripts/conformance-badge.sh json native-conformance "$native" "$v" \
      > docs/badges/native-conformance.json.tmp
    mv docs/badges/conformance.json.tmp docs/badges/conformance.json
    mv docs/badges/jvm-byte-equality.json.tmp docs/badges/jvm-byte-equality.json
    mv docs/badges/native-conformance.json.tmp docs/badges/native-conformance.json
    printf '{"schemaVersion":1,"label":"Kotlin","message":"%s","color":"blue"}\n' \
      "$v" > docs/badges/kotlin.json
    echo "wrote docs/badges/conformance.json + jvm-byte-equality.json + native-conformance.json + kotlin.json"

# Run the box-corpus survey — the roadmap of why krusty SKIPS a codegen/box test (the unresolved /
# unsupported buckets, most-frequent first). Provisions the SAME version-matched, cached corpus +
# toolchain the conformance gate uses (never a random local checkout), so the buckets reflect the
# supported Kotlin version. Optional `[CATEGORY]` lists the files in a matching bucket.
survey *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    v="$(just max-version)"
    export KRUSTY_KOTLINC="${KRUSTY_KOTLINC:-$(just kotlinc "$v")}"
    box="${KRUSTY_KOTLIN_BOX_DIR:-$(just box-corpus "$v")}"
    cargo build --release --bin survey >&2
    if [ -n "{{ARGS}}" ]; then
        ./target/release/survey "$box" --samples {{ARGS}}
    else
        ./target/release/survey "$box"
    fi

# Install git hooks via lefthook (reads lefthook.yml). Needs lefthook on PATH.
install-hooks:
    @command -v lefthook >/dev/null 2>&1 || { echo "lefthook not found — install it: https://lefthook.dev (e.g. 'go install github.com/evilmartians/lefthook@latest' or your package manager)" >&2; exit 1; }
    lefthook install
    @echo "git hooks installed via lefthook"

# --- versioning: krusty's release version = supported Kotlin reference version + build number ---
#
# The kotlin-versions manifest lists the Kotlin reference versions (full major.minor.patch, e.g.
# 2.4.20) krusty is validated against. The MAX is krusty's headline release version; every entry is
# also tested in parallel by CI against that kotlinc. The build number resets per reference version
# (commits since its baseline), so 2.4.20-build.3 is the 3rd krusty build supporting Kotlin 2.4.20.

# Supported Kotlin reference versions, one per line.
kotlin-versions:
    @grep -vE '^\s*(#|$)' {{manifest}} | awk '{print $1}'

# Comma-separated list of supported Kotlin reference versions (advertised by `krusty -version`).
supported-kotlin:
    @grep -vE '^\s*(#|$)' {{manifest}} | awk '{print $1}' | sort -V | paste -sd, -

# Highest supported Kotlin reference version — krusty's release version (without the build suffix).
max-version:
    @grep -vE '^\s*(#|$)' {{manifest}} | awk '{print $1}' | sort -V | tail -1

# JSON array of supported Kotlin reference versions, for the GitHub Actions test matrix.
matrix-json:
    @grep -vE '^\s*(#|$)' {{manifest}} | awk 'BEGIN{printf "["} {printf "%s\"%s\"",(NR>1?",":""),$1} END{print "]"}'

# Build number for a Kotlin reference version: commits since its baseline + 1 (resets per version,
# reproducible from any clone, no CI state).
build-number VERSION:
    #!/usr/bin/env bash
    set -euo pipefail
    base=$(awk -v k="{{VERSION}}" '$1==k && $1!~/^#/ {print $2; f=1} END{if(!f) print "__missing__"}' {{manifest}})
    if [ "$base" = "__missing__" ]; then
        echo "unknown Kotlin reference version: {{VERSION}} (not in {{manifest}})" >&2
        exit 1
    fi
    if [ "$base" = "-" ] || [ -z "$base" ]; then
        git rev-list --count HEAD
    else
        echo $(( $(git rev-list --count "${base}..HEAD") + 1 ))
    fi

# Full krusty release version: <max-reference-version>-build.<n>  (e.g. 2.4.20-build.3).
# SemVer prerelease, so builds are strictly ordered (2.4.20-build.3 > 2.4.20-build.2).
version:
    #!/usr/bin/env bash
    set -euo pipefail
    v=$(just max-version)
    echo "${v}-build.$(just build-number "$v")"

# Build optimized compiler and LSP executables from their independent workspace packages.
build-release TARGET="":
    #!/usr/bin/env bash
    set -euo pipefail
    export KRUSTY_VERSION="$(just version)"
    export KRUSTY_KOTLIN_SUPPORT="$(just supported-kotlin)"
    if [ -n "{{TARGET}}" ]; then
        cargo build --release --target {{TARGET}} -p krusty-cli --bin krusty
        cargo build --release --target {{TARGET}} -p krusty-lsp
    else
        cargo build --release -p krusty-cli --bin krusty
        cargo build --release -p krusty-lsp
    fi
    echo "built krusty and krusty-lsp $KRUSTY_VERSION (supported Kotlin: $KRUSTY_KOTLIN_SUPPORT) ${TARGET:+for {{TARGET}}}"

# Package one executable into dist/ (.tar.gz on unix, .zip on windows). PRODUCT defaults to the
# compiler for compatibility; release CI invokes this once for `krusty` and once for `krusty-lsp`.
package TARGET PRODUCT="krusty":
    #!/usr/bin/env bash
    set -euo pipefail
    ver=$(just version)
    product="{{PRODUCT}}"
    case "$product" in
        krusty|krusty-lsp) ;;
        *) echo "unknown release product: $product" >&2; exit 2 ;;
    esac
    bindir="target/{{TARGET}}/release"
    name="${product}-${ver}-{{TARGET}}"
    mkdir -p dist
    if [[ "{{TARGET}}" == *windows* ]]; then
        out="$PWD/dist/${name}.zip"
        rm -f "$out"
        if command -v 7z >/dev/null 2>&1; then
            ( cd "$bindir" && 7z a -tzip "$out" "${product}.exe" >/dev/null )
        else
            ( cd "$bindir" && zip -q "$out" "${product}.exe" )
        fi
        echo "dist/${name}.zip"
    else
        tar -C "$bindir" -czf "dist/${name}.tar.gz" "$product"
        echo "dist/${name}.tar.gz"
    fi

# Package the Zed extension source into dist/.
package-zed:
    #!/usr/bin/env bash
    set -euo pipefail
    ver=$(just version)
    src="editors/zed"
    staging_root="$(mktemp -d)"
    trap 'rm -rf "$staging_root"' EXIT
    staging="$staging_root/krusty-zed"
    mkdir -p "$staging"
    cp -R "$src/." "$staging/"
    rm -rf "$staging/target"
    sed -i.bak -E "s/^version = \".*\"/version = \"$(just max-version)\"/" "$staging/extension.toml"
    rm -f "$staging/extension.toml.bak"
    mkdir -p dist
    tar -C "$(dirname "$staging")" -czf "dist/krusty-zed-${ver}.tar.gz" "krusty-zed"
    echo "dist/krusty-zed-${ver}.tar.gz"
