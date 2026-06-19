# krusty build-system entrypoint.
#
# `just` is the single command CI and contributors share — `just ci` is the whole PR gate, and the
# release pipeline reuses these recipes, so anything CI does can be reproduced locally.
#
#   just            list recipes
#   just ci         PR gate: lint + test
#   just lint       fmt-check + clippy-baseline-check (fails on NEW clippy findings)
#   just fmt        apply rustfmt
#   just clippy-baseline   refreeze the accepted clippy findings (clippy-baseline.tsv)
#   just test       full test suite (optionally `just test -- <args>`)
#   just test-all   suite against every supported Kotlin version, in parallel
#   just kotlinc    download+unpack the reference kotlinc dist; prints bin path
#   just box-corpus clone+cache the Kotlin codegen/box corpus; prints box dir
#   just conformance       print box-suite conformance "<pct> <passed> <scanned>"
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

manifest := "kotlin-versions"

# List available recipes.
default:
    @just --list

# === PR gate: the one command CI runs ===
ci: lint test-all

# Lint gate (enforced locally + in CI + by the pre-commit hook): formatting must be clean, and
# clippy must introduce NO new findings beyond the frozen baseline (clippy-baseline.tsv). Existing
# findings are tolerated; any new one fails. Identical behaviour everywhere — it's plain cargo + sh.
lint: fmt-check clippy-baseline-check

# rustfmt must be clean (the repo is fully formatted; `just fmt` fixes any drift).
fmt-check:
    cargo fmt --all --check

# Apply rustfmt across the workspace.
fmt:
    cargo fmt --all

# Emit current clippy findings as a stable, line-number-independent fingerprint set:
#   <count><TAB><file><TAB><message>   (one per file+message, so a new occurrence bumps the count)
clippy-findings:
    @cargo clippy --all-targets --all-features --message-format=short 2>&1 \
      | grep -E ':[0-9]+:[0-9]+: warning: ' \
      | sed -E 's/^([^:]+):[0-9]+:[0-9]+: warning: (.*)$/\1\t\2/' \
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

# Full test suite against the default toolchain (kotlinc-gated tests skip without KRUSTY_KOTLINC).
test *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    v="$(just max-version)"
    # Provision the reference toolchain + box corpus (cached, idempotent) and export them, so the
    # conformance + box e2e tests run rather than fail on a missing env. Honor any ambient overrides.
    export KRUSTY_KOTLINC="${KRUSTY_KOTLINC:-$(just kotlinc "$v")}"
    export KRUSTY_KOTLIN_BOX_DIR="${KRUSTY_KOTLIN_BOX_DIR:-$(just box-corpus "$v")}"
    cargo test {{ARGS}}

# Download + unpack the reference Kotlin compiler distribution into one self-contained dir
# (.kotlinc/<ver>/), and print the path to its `bin/kotlinc`. Idempotent — a no-op once unpacked
# (so it's cheap to cache). This is the reference toolchain the differential harness validates
# against; its `lib/kotlin-stdlib.jar` is also what the box e2e tests put on the runtime classpath.
# Point KRUSTY_KOTLINC at the printed path:  export KRUSTY_KOTLINC="$(just kotlinc)"
kotlinc VERSION=`just max-version`:
    #!/usr/bin/env bash
    set -euo pipefail
    ver="{{VERSION}}"
    dest="$PWD/.kotlinc/$ver"
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

# Provision the Kotlin codegen/box conformance corpus into one cached dir (.kotlin-box/<ver>/) and
# print the path to compiler/testData/codegen/box. Blobless + sparse clone of just that directory at
# the matching tag — small and idempotent (no-op once present, cheap to cache). Mirrors `kotlinc`:
# the conformance test FAILS (not skips) without it, so the harness provisions it rather than
# silently skipping. Point KRUSTY_KOTLIN_BOX_DIR at the printed path.
box-corpus VERSION=`just max-version`:
    #!/usr/bin/env bash
    set -euo pipefail
    ver="{{VERSION}}"
    root="$PWD/.kotlin-box/$ver"
    box="$root/compiler/testData/codegen/box"
    if [ -d "$box" ]; then echo "$box"; exit 0; fi
    echo "cloning Kotlin codegen/box corpus (v${ver})…" >&2
    rm -rf "$root"
    git clone --depth 1 --filter=blob:none --sparse --branch "v${ver}" \
        https://github.com/JetBrains/kotlin.git "$root" >&2 \
        || { echo "failed to clone JetBrains/kotlin v${ver}" >&2; rm -rf "$root"; exit 1; }
    git -C "$root" sparse-checkout set compiler/testData/codegen/box >&2
    [ -d "$box" ] || { echo "box dir missing after sparse checkout: $box" >&2; exit 1; }
    echo "$box"

# Run the full suite against EVERY supported Kotlin reference version, in parallel — locally and in
# CI alike. Parallelization lives here, not in a CI matrix, so `just test-all` behaves identically
# everywhere and needs no GitHub-Actions infra. Each version gets its own CARGO_TARGET_DIR (so the
# concurrent cargo runs don't fight over the build lock) and its own log; KRUSTY_LANGUAGE_VERSION is
# exported for the differential harness. Set KRUSTY_KOTLINC_<ver> (dots->underscores) to point a
# version at a specific kotlinc, else the vendored dist from `just kotlinc` (.kotlinc/<ver>/...) is
# used automatically, else the ambient KRUSTY_KOTLINC.
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
        vendored="$PWD/.kotlinc/$v/kotlinc/bin/kotlinc"
        [ -z "$kc" ] && [ -x "$vendored" ] && kc="$vendored"
        [ -z "$kc" ] && kc="${KRUSTY_KOTLINC:-}"
        ( CARGO_TARGET_DIR="target/kt-$v" \
          KRUSTY_LANGUAGE_VERSION="$v" \
          KRUSTY_KOTLINC="$kc" \
          KRUSTY_KOTLIN_BOX_DIR="$PWD/.kotlin-box/$v/compiler/testData/codegen/box" \
          cargo test {{ARGS}} > "target/test-$v.log" 2>&1 ) &
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

# --- Kotlin box-suite conformance (drives the README badge) ---

# Run the codegen/box conformance suite and print "<pct> <passed> <scanned>". Auto-provisions the
# reference kotlinc + box corpus (cached) so it never silently skips. Coverage <100% is fine — the
# gate is "never miscompile an accepted case", not a percentage; the % is informational (the badge).
conformance:
    #!/usr/bin/env bash
    set -euo pipefail
    v="$(just max-version)"
    export KRUSTY_KOTLINC="${KRUSTY_KOTLINC:-$(just kotlinc "$v")}"
    export KRUSTY_KOTLIN_BOX_DIR="${KRUSTY_KOTLIN_BOX_DIR:-$(just box-corpus "$v")}"
    out=$(cargo test --release --test kotlin_box_ir_jvm_conformance -- --nocapture 2>&1 || true)
    line=$(printf '%s\n' "$out" | grep -E 'box\(\)=OK:' | tail -1)
    [ -n "$line" ] || { echo "no conformance summary — set KRUSTY_KOTLIN_BOX_DIR and JAVA_HOME" >&2; exit 1; }
    scanned=$(printf '%s' "$line" | sed -E 's/.*scanned: ([0-9]+).*/\1/')
    passed=$(printf '%s' "$line" | sed -E 's/.*box\(\)=OK: ([0-9]+).*/\1/')
    printf '%s %s %s\n' "$(awk -v p="$passed" -v s="$scanned" 'BEGIN{printf "%.1f", (s>0)?100*p/s:0}')" "$passed" "$scanned"

# Write the shields.io endpoint badges (docs/badges/*.json) from current numbers. CI commits these
# on master; safe to run locally to preview. Color ramps with the conformance percentage.
conformance-badge:
    #!/usr/bin/env bash
    set -euo pipefail
    read -r pct passed scanned < <(just conformance)
    color=red
    awk "BEGIN{exit !($pct>=10)}" && color=orange || true
    awk "BEGIN{exit !($pct>=40)}" && color=yellow || true
    awk "BEGIN{exit !($pct>=70)}" && color=brightgreen || true
    mkdir -p docs/badges
    printf '{"schemaVersion":1,"label":"Kotlin %s conformance","message":"%s%% (%s/%s)","color":"%s"}\n' \
      "$(just max-version)" "$pct" "$passed" "$scanned" "$color" > docs/badges/conformance.json
    printf '{"schemaVersion":1,"label":"Kotlin","message":"%s","color":"blue"}\n' \
      "$(just max-version)" > docs/badges/kotlin.json
    echo "wrote docs/badges/conformance.json + kotlin.json"

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

# Build an optimized krusty. Optional TARGET is a rust target triple.
build-release TARGET="":
    #!/usr/bin/env bash
    set -euo pipefail
    export KRUSTY_VERSION="$(just version)"
    export KRUSTY_KOTLIN_SUPPORT="$(just supported-kotlin)"
    if [ -n "{{TARGET}}" ]; then
        cargo build --release --target {{TARGET}}
    else
        cargo build --release
    fi
    echo "built krusty $KRUSTY_VERSION (supported Kotlin: $KRUSTY_KOTLIN_SUPPORT) ${TARGET:+for {{TARGET}}}"

# Package the built binary into dist/ (.tar.gz on unix, .zip on windows). Prints the archive path.
package TARGET:
    #!/usr/bin/env bash
    set -euo pipefail
    ver=$(just version)
    bindir="target/{{TARGET}}/release"
    name="krusty-${ver}-{{TARGET}}"
    mkdir -p dist
    if [[ "{{TARGET}}" == *windows* ]]; then
        out="$PWD/dist/${name}.zip"
        rm -f "$out"
        if command -v 7z >/dev/null 2>&1; then
            ( cd "$bindir" && 7z a -tzip "$out" krusty.exe >/dev/null )
        else
            ( cd "$bindir" && zip -q "$out" krusty.exe )
        fi
        echo "dist/${name}.zip"
    else
        tar -C "$bindir" -czf "dist/${name}.tar.gz" krusty
        echo "dist/${name}.tar.gz"
    fi
