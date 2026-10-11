# Test Harness

Use `./run-tests.sh` as the canonical test entrypoint. It is self-provisioning and normally needs no
parameters.

## Agent Quick Reference

- Use `./run-tests.sh` for the full suite; it provisions kotlinc and the Kotlin codegen/box corpus.
- **Without `just`, provision kotlinc by hand — do not proceed without it.** `run-tests.sh` reaches
  the reference compiler through `just kotlinc <ver>`, so in an environment that has no `just` every
  differential test fails with `reference compiler unavailable` and a run of them proves nothing. The
  recipe is a plain download, so do what it does:

  ```sh
  ver=2.4.20
  dest="$PWD/target/cache/kotlinc/$ver"
  mkdir -p "$dest"
  curl -fsSL "https://github.com/JetBrains/kotlin/releases/download/v${ver}/kotlin-compiler-${ver}.zip" -o /tmp/kotlinc.zip
  python3 -c "import zipfile; zipfile.ZipFile('/tmp/kotlinc.zip').extractall('$dest')"
  chmod +x "$dest/kotlinc/bin/"*
  export KRUSTY_KOTLINC="$dest/kotlinc/bin/kotlinc"
  ```

  `KRUSTY_KOTLINC` is what the harness reads, so exporting it is the whole of the setup. This matters
  more than it looks: kotlinc is the correctness oracle (`docs/SPEC.md` §6), so a change validated
  without it has been checked against an expectation rather than against the reference — which is
  exactly the mistake the differential harness exists to prevent.
- Use focused harness runs, not raw `cargo test`, while iterating. Standalone suites still use `./run-tests.sh --test <name> -- --nocapture`; grouped e2e tests use a test-name filter, e.g. `./run-tests.sh --test e2e lambda_e2e::lambdas_run -- --nocapture`.
- Use `./run-tests.sh --survey --parse-only --report /tmp/krusty-parse.tsv` for the syntax-only
  conformance gate. It parses every Kotlin block independently, ignores Java fixture blocks, checks
  AST integrity, and never enters signature collection or checking.
- Use `./run-tests.sh --survey --frontend-only` to audit parser/signature/checker skips against the
  pinned corpus without building or running the backend. This is deliberately not a parser metric.
- Do not pass `--release`; the gate profile is the intended fast edit/build/test loop. The `krusty`
  package, including its integration tests, is opt-level 2 with 16 codegen units, because the box
  corpus and e2e run that compiler in-process. A one-line incremental rebuild measured about 8s.
- `gate` is a Cargo *profile* (`--profile gate`), never a target directory. `--target-dir target/gate`
  or `CARGO_TARGET_DIR=target/gate` silently builds the *dev* profile into `target/gate/debug`; the
  harness refuses that shape. It also prunes orphan `*.rcgu.o` codegen temporaries (left by any rustc
  killed mid-build) older than six hours before building — cargo never garbage-collects `target/`
  itself, so stale profile/feature variants still need an occasional
  `cargo clean -p krusty -p krusty-cli -p krusty-lsp --profile gate` (drops only workspace crates,
  keeps third-party deps).
- For Kotlin box conformance changes, run `./run-tests.sh --test conformance kotlin_codegen_box_conformance -- --nocapture` and keep `FAIL: 0`.
- For performance work, start with the harness timing output or `KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1`.

## Normal Runs

```sh
./run-tests.sh
```

The LSP crate also has an opt-in protocol differential against JetBrains' official Kotlin LSP. It
compares normalized diagnostic ranges, severity, source, and messages, decoded semantic-token types
and modifiers, exact definition and type-definition target URIs/ranges, complete sorted transitive
implementation locations, find-reference declaration filtering and location sets, complete hover
markdown/ranges, and stable completion labels, kinds, label details, ranking, and incomplete status.
Every navigation comparison checks both UTF-16 endpoints; matching text at the wrong location fails.
Implementation coverage includes class and member declarations, references, generic substitution,
overload selection, `null` leaf results, and a query following a supplementary-plane character.
Rename compares the complete `WorkspaceEdit`, including document URI/version, edit ordering,
replacement text, and both UTF-16 range endpoints for cross-file, lexical, overload-selected,
Unicode-offset, and backticked identifiers.
Hierarchical document symbols compare the complete protocol value, including order, nesting, names,
kinds, deprecation/tags, full ranges, and selection ranges. A correct symbol at the wrong location
therefore fails just like a diagnostic at the wrong location.
The same differential requires incremental synchronization capability and applies ordered ranged
edits before comparing the resulting definition URI and exact UTF-16 range.
The test does not compare raw protocol token indexes whose legends can differ or implementation
specific completion commands and opaque data. Point the environment variable at an installed
official launcher; the regular suite does not download the roughly 400 MB, platform-specific
distribution. The differential creates a minimal Gradle project using the highest version in
`kotlin-versions`, because current official servers do not analyze loose source files without a
workspace model:

```sh
KRUSTY_KOTLIN_LSP=/path/to/bin/intellij-server \
./run-tests.sh -p krusty-lsp --test kotlin_lsp_diff -- --nocapture
```

The compiler diagnostic differential uses the provisioned kotlinc and compares each first error's
source filename, 1-based line and column, and exact message. A matching message at the wrong call,
argument, member, initializer, or assignment location is a test failure.

`just test` is equivalent. When `just` is available, the harness provisions the matching Kotlin
compiler and codegen/box corpus, exports `KRUSTY_KOTLINC` and `KRUSTY_KOTLIN_BOX_DIR`, builds the test
binaries once with Cargo's `gate` profile, runs the conformance binary alone in five passes (JVM
box corpus, Native box corpus, the `wasm-js` and `wasm-wasi` box corpora, then everything else), then runs the internally parallel e2e binary
once, then runs the remaining small test binaries in parallel. The e2e suite is one process under
`KRUSTY_E2E_TIMEOUT_SECONDS`. The native codegen/box lane also runs once, excluded from the
"everything else" pass, under its dedicated `KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS` suite
deadline. `scripts/box-lane-run.sh <native|wasm-js|wasm-wasi>` owns that same invocation for the
`just` recipes and CI (`scripts/native-conformance-run.sh` is its Native shorthand).

The two Wasm lanes compile each case to one WasmGC module plus its `.mjs` loader and run it under
Node.js 22 or newer: `wasm-js` through JavaScript imports, `wasm-wasi` through Node's WASI preview-1
host. They still analyze each case against the JVM library surface (`kotlin-stdlib.jar` and the JDK),
because no Wasm klib platform exists yet, so they gate the Wasm emitter, not wasm-js/wasm-wasi source
semantics, and publish no badge. Without a Node.js 22+ the local run skips them with a notice; CI
requires them (`KRUSTY_REQUIRE_WASM_JS_CONFORMANCE`, `KRUSTY_REQUIRE_WASM_WASI_CONFORMANCE`). Their
knobs mirror Native's with the `WASM_JS`/`WASM_WASI` infix (`KRUSTY_WASM_JS_BOX_ONLY`,
`…_BOX_LIMIT`, `…_BOX_TRACE`). Each lane has its own ratchet, keyed by `wasm-js` or `wasm-wasi`,
because the corpus mutes the two targets separately.

The Native lane uses the same committed text ratchet as JVM, keyed by `native` and the exact Kotlin
release. Backend declines, frontend rejections, and accepted-but-wrong executions are expected
failures. A missing harness capability, including an unwired multi-module topology, is also an
expected failure and remains in the denominator. Only target/source-universe exclusions are
not-applicable. Pass is implicit. Any new non-pass or
transition between pass/fail/not-applicable fails, and compiler panics always fail. No version range,
GHA cache, or bootstrap exists: a local checkout and CI read the same reviewable baseline.
A `box()` answer is read from behind the entry's `BOX_RESULT_FRAME` marker, whole, and never from the
last line of output. A scheduled invocation requires the prebuilt runtime and the provisioned corpus;
only a plain local run may skip or fall back to the vendored cases.

Each scheduled invocation owns its log. An unfiltered binary keeps the plain `<binary>.log` name; a
filtered invocation appends an `@<filter-slug>` derived by `run_label`, so the two conformance logs are
`conformance-<hash>@kotlin_codegen_box_conformance.log` and
`conformance-<hash>@skip-kotlin_codegen_box_conformance.log`. Scheduling-only
`--test-threads=<count>` arguments do not change identity. Because slugging is deliberately lossy,
`run_one` adds `#2`, `#3`, and so on if a derived name is already present instead of overwriting an
earlier run. The failure report reads the exact invocation's log, and the timing table lists each
invocation separately because each is a separate process with its own wall time. The e2e log is the
unfiltered binary name.

CI builds the conformance test binary and CLI once. Every version in `kotlin-versions` expands into
independent `jvm` and `native` matrix rows from those artifacts. The JVM row runs its box corpus and
every active non-box conformance test; the Native row runs its box corpus independently, so neither
target waits for the other's corpus result. `KRUSTY_LANGUAGE_VERSION`, `KRUSTY_KOTLINC`, and
`KRUSTY_KOTLIN_BOX_DIR` select the runtime reference toolchain, so the matrix does not rebuild Rust
code per Kotlin version or target. Toolchain and box/serialization corpora are isolated by version.
JVM rows opt into KSP and require both KSP and pinned serialization-runtime
provisioning, so missing prerequisites fail rather than turning those tests into passing skips. Each
row uses its target's configurable process-group deadline. JVM rows upload `pct-<version>` and
`jvm-byte-equality-<version>`; Native rows upload `native-pct-<version>`. A release publishes only
after all target rows pass and every row matches its platform/version expectations.

### Box-test JDK

Box tests compile against the JDK JetBrains' runner gives them (`JvmEnvironmentConfigurator`): the
Java 7 mock `rt.jar` with no ambient JDK, unless the test declares `// FULL_JDK`, which selects the
running JDK's `lib/modules`. Tests always run on the real JVM. `krusty::conformance::BoxJdkRoots`
owns that selection for the gate, the survey, the reference-compiler oracle (`-no-jdk -classpath
<mock rt.jar>`), and byte-diff mode. `just box-corpus` checks the mock JDK out of the corpus
checkout at the same tag (`third-party/mockJDKs/mockJDK` from 2.4.20, `compiler/testData/mockJDK`
before) and extends an older cache that lacks it; a run without it fails instead of compiling
against another JDK. A mock-JDK run cannot see divergences that only appear against a modern JDK
surface, so those need their own repository tests.

## Box Outcome Ratchet

The generic ratchet is keyed by platform and exact Kotlin version:
`tests/box_expected_failures/<platform>/<version>.txt` and
`tests/box_expected_not_applicable/<platform>/<version>.txt`. Each entry is a corpus-relative path;
pass is implicit. JVM has no expected failures, so every applicable JVM case must pass. Native's
known non-passing backlog is explicit. Every pass/fail/not-applicable transition, duplicate, and
corpus-absent entry fails. Sharded and filtered runs judge only their scheduled files while still
validating committed paths against the complete corpus.

After a change that moves any outcome, rewrite both manifests from a full run and commit the diffs
with the change, so review sees exactly which files moved:

```sh
# JVM applicability
KRUSTY_BLESS_BOX_EXPECTATIONS=1 KRUSTY_LANGUAGE_VERSION=<v> \
  ./run-tests.sh --test conformance kotlin_codegen_box_conformance -- --nocapture

# Native failures and applicability (requires the provisioned Native runtime/corpus)
KRUSTY_BLESS_BOX_EXPECTATIONS=1 KRUSTY_LANGUAGE_VERSION=<v> \
  ./run-tests.sh --test conformance kotlin_codegen_box_native_conformance -- --nocapture

# Wasm failures and applicability, one lane per target (requires Node.js 22+)
KRUSTY_BLESS_BOX_EXPECTATIONS=1 KRUSTY_LANGUAGE_VERSION=<v> \
  ./run-tests.sh --test conformance kotlin_codegen_box_wasm_js_conformance -- --nocapture
KRUSTY_BLESS_BOX_EXPECTATIONS=1 KRUSTY_LANGUAGE_VERSION=<v> \
  ./run-tests.sh --test conformance kotlin_codegen_box_wasm_wasi_conformance -- --nocapture
```

Blessing requires the exact value `1` and is refused under CI and on a partial run. Each tracked file
is replaced atomically. Review and commit the platform/version diff. The failure lists only shrink;
the long-term goal is zero expected failures on every platform.

A pull request is judged against its own base, so two that each match the lists can still disagree
with them once both land: one fixes a file the other's list still names, or their changes interact.
The `ci` workflow also runs on `merge_group`, so with master's merge queue (or "require branches to
be up to date") on and the `ci-and-conformance` check required (see "Required Check"), every merge
is checked on the combined commit before master moves. A branch that falls behind merges master in
and re-blesses.

All existing platform/version manifests only shrink. The required `ci` job runs
`scripts/check-outcome-lists.sh` before building and fails a pull request that adds an entry to any
existing outcome manifest compared with its merge base, including one entry swapped for another:
a regression is fixed, not recorded, and a fix does not pay for one. A manifest for a newly
supported Kotlin version is exempt. Native and JVM inventories use the same script and repository
layout, and it covers the CLI corpus's expected failures (`tests/cli_expected_failures`) too. The CLI
not-applicable lists are not under this gate: they record what the reference kotlinc cannot run in
this environment, and the required reference run proves every entry against that compiler.

The rest of the suite is version-sensitive too, because the supported kotlinc releases do not word
every diagnostic alike (see `docs/SPEC.md` §6). `KRUSTY_LANGUAGE_VERSION=<v> ./run-tests.sh` runs the
whole suite with krusty reproducing release `<v>` against that release's kotlinc; `just test-all`
does it for every manifest version at once.

A diagnostic differential compares against the selected kotlinc observation directly.
`common::assert_errors_match_kotlinc` (the whole `file:line:column: message` ledger through both
CLIs) and `common::assert_messages_match_kotlinc` (the frontend's messages) cover the usual shapes.
Kotlinc exit status and stderr are part of the binary invocation recording described below, so no
duplicate diagnostic ledger is committed to the repository. Language and API options are ordinary
invocation inputs and therefore select distinct recordings automatically.

Byte-equality checks (`byte_diff_against_kotlinc`, `compare_with_kotlinc_plugin`,
`compile_with_kotlinc`, `classes_against_kotlinc_lib`), metadata differentials, and diagnostic
invocations share one binary archive in `KRUSTY_CLASS_DUMP_DIR`, which defaults to
`target/cache/class-dumps/`. The archive is a single zlib stream: a text index, then each distinct
output once. A run keeps new dumps in memory and writes that file once, when the process exits. An
index entry contains one exact compiler version, a content fingerprint, and a blob id. Release and
RC channels use separate exact-version slots. A dump is used only when its fingerprint and exact
compiler version match. The lookup fingerprint
covers source bytes, target, flags, and input identities; it is distinct from the blob id that
hashes the recorded compiler output. Every lookup starts with content-derived identities for the
selected kotlinc installation's complete `lib/` tree and the selected JDK's exact `modules` image,
even when the explicit classpath is empty or unrelated. Selected kotlinc classpath entries then
contribute their logical distribution paths. Explicit selected JDK `modules` and `ct.sym` entries
contribute their kind, exact image-content identity, and the hashed label from that JDK's `release`
file. These selected identities are memoized once per canonical immutable installation during a
test process, so individual fixtures do not repeatedly read large platform artifacts. Every other
arbitrary dependency remains content-hashed. The key contains no installation path: equal toolchain
bytes in two locations match, while a patched compiler/runtime or JDK image with the same reported
version is a hard miss.
An older archive may still index a line as `2.4.20..`. That line is replayed for 2.4.20 only. A
lookup that misses the current fingerprint also tries the earlier classpath identity, which hashed
explicit entries and did not include the selected kotlinc or JDK. New recordings keep the current
fingerprint and an exact version. A file this process cannot parse is replaced when the run
publishes, instead of being saved unchanged.
Locally, a release (`2.4.20`, `2.4.20-release-482`) or an RC tag
(`2.4.20-RC`, `2.4.20-RC2`, `2.4.0-RC-137`) with no matching dump fails the test and does not run
kotlinc. `KRUSTY_RECORD_CLASS_DUMPS=1` recompiles and replaces the exact-version entries the run reaches. GitHub
restores an immutable cache for the PR's master base.
PR and merge-group jobs replay every matching entry, compile missing or incomplete entries live,
and never save the result. Thus a partial prefix cache is an accelerator, not an authority. A
successful master job refreshes and saves the cache under the supported Kotlin version and master
commit. A snapshot, dev, or beta build never reads or writes dumps — that version string is not a
stable artifact, so it still compiles. Every kotlinc call except a box-corpus reference compile goes
through the same archive. A successful
build and a rejected one both keep the exit code and kotlinc's
diagnostics, and an assert replays them. Locally, a dump that has class files but no exit code or
diagnostics fails that assert instead of compiling; read-only CI compiles that incomplete entry
live. The archive is read at runtime and is not compiled into the test binary or committed to the repository.
The corpus byte-diff cache under `target/cache/ref-classes/` follows
the same release/RC rule and stays uncached for any other compiler. A box-corpus reference compile
reads and writes only that cache: its key already covers the source, stem, classpath, each
compilation unit's code-generation modes, the compiler identity, and the producing JDK, so a miss
compiles live with kotlinc in every environment and never touches the archive. Each unit compiles
under the `// LAMBDAS:`, `// SAM_CONVERSIONS:`, and `// JVM_DEFAULT_MODE:` modes its own Kotlin
sources select, exactly as krusty's gate compile selects them, and an unrecognized mode value is a
reference failure. The producing JDK is the one `KRUSTY_REF_JAVA_HOME` (else `JAVA_HOME`) selects for
the reference kotlinc server and javac; it is identified by its `release` record and the size and
modification time of its `lib/modules` image, so retargeting or upgrading a JDK at the same path
still recompiles. Set `KRUSTY_SECOND_JAVA_HOME` to a JDK of another feature release to make
`a_changed_producing_jdk_misses_the_reference_cache_across_processes` also check the cross-JDK miss.
Recording each of those compiles into the archive as well rewrote the whole archive per store and
pushed a cold scored run past its deadline.

A live kotlinc invocation writes one line straight to the test process's stderr (libtest's capture
does not hide it), and a passing replay writes nothing:

```text
class-dump: live kotlinc cache-miss test=<module>::<case> sources=<files> fingerprint=<hex>
```

`record` replaces `cache-miss` when `KRUSTY_RECORD_CLASS_DUMPS=1` (or `KRUSTY_RECORD=1`) recompiles
a stored entry. `uncached-compiler` replaces it when the selected kotlinc is not a release or RC, so
that compiler never reads the archive. A box-corpus reference compile reports the same line on each
`target/cache/ref-classes/` miss; that cache ignores a forced re-record, so its reason is `cache-miss`
or `uncached-compiler`, and its `test` is `unknown` because box cases compile on unnamed worker
threads (`sources` names the case's files). When the process exits, one summary names each test that
compiled live and how many invocations it made:

```text
class-dump: live kotlinc summary invocations=<n> tests=<n>
class-dump: live kotlinc summary cache-miss test=<module>::<case> invocations=<n>
```

## CLI Corpus Ratchet

`kotlin_cli_jvm_conformance` (in the `conformance` binary) runs Kotlin's own command-line test
corpus, `compiler/testData/cli/jvm` at the reference tag, through the krusty binary. `just box-corpus`
provisions it in the same checkout as the box corpus. CI caches that checkout under a key versioned
for every input it carries (`box-and-cli-corpus-v1-…`); a cache hit sets `KRUSTY_BOX_CORPUS_OFFLINE=1`,
and `just box-corpus` then fails rather than fetch into an incomplete restored checkout, so adding a
corpus input means bumping the salt. Each case is an `.args` file and the `.out`
file kotlinc's `AbstractCliTest` compares against: the normalized stderr followed by the exit code's
name, so the corpus covers argument parsing, help text, configuration errors, source discovery and
reported diagnostics.

`tests/cli_expected_failures/jvm/<version>.txt` lists the cases krusty does not reproduce yet; any
case joining or leaving it fails. Rewrite it from a full local run:

```sh
KRUSTY_BLESS_CLI_EXPECTATIONS=1 KRUSTY_LANGUAGE_VERSION=<v> \
  ./run-tests.sh --test conformance kotlin_cli_jvm_conformance -- --nocapture
```

`tests/cli_expected_not_applicable/jvm/<version>.txt` lists the cases the reference kotlinc does not
reproduce outside JetBrains' environment, and the cases no released compiler can run (the `.env`
cases), each under a comment saying why. `the_reference_compiler_reproduces_every_applicable_cli_case`
runs kotlinc through the same runner: it must pass every unlisted case and no listed one. It takes
minutes, so ordinary runs ignore it; `scripts/conformance-regressions.sh`, which the conformance job
runs for every supported release, runs it explicitly under `KRUSTY_CLI_REFERENCE_TIMEOUT_SECONDS`.

The corpus names JDK 8, 11, 17 and 21 homes. A case whose JDK the machine lacks fails rather than
being skipped, so the gate never tests fewer cases than it claims. JDKs are found under
`/usr/lib/jvm` and `/Library/Java/JavaVirtualMachines`, from `JAVA_HOME`, from the
`JAVA_HOME_<release>_X64` variables `actions/setup-java` exports, and from
`KRUSTY_JDK_<release>_HOME`, which overrides the rest.

## JVM processes

kotlinc, `javac`, and `java` that the suite runs on every test are pooled. A cache hit does not
start kotlinc. A miss uses a persistent compiler JVM (`KRUSTY_SERVER_POOL` caps the pool). `box()`
and Java drivers use persistent `BoxRunner` and `JavaRunner` JVMs; `javac` runs once per runner
source to compile that helper, then in-process. The conformance box runner is a separate persistent
JVM per test thread, also compiled once.

`javap` is not a process on that path. Disassembly goes through `JavaRunner`'s in-process tool, and
only to explain a failure. The oracle for a class is byte-for-byte equality with kotlinc's class
file. A well-formedness check reads the class file and disassembles when that read fails. A
method-code comparison passes when the class files are identical and disassembles them only when
they are not. The survey's reference acceptance oracle starts a kotlinc process per case; it is not
part of `just ci`.

The general test-binary deadline defaults to 120 seconds. Each conformance pass defaults to 120
seconds, including each shard of the scored byte-equality run, and can be adjusted with
`KRUSTY_CONFORMANCE_TIMEOUT_SECONDS`. The product e2e suite is one process and defaults to 1800
seconds (`KRUSTY_E2E_TIMEOUT_SECONDS`).

Do not use `--release` for tests. The release build cycle takes longer than it saves at runtime, and
`run-tests.sh --release` is rejected intentionally.

## Required Check

The `ci` workflow's `ci-and-conformance` job is the single check for master's ruleset to require.
It needs core `ci`, `klib-semantics`, the whole `conformance` matrix, `build-gradle-plugin`, and the
whole `gradle` compatibility matrix. Every supported Kotlin version has distinct JVM and Native
conformance rows; the JVM row also owns the non-box suite. The job is scheduled with `if: always()`,
because a required check that is skipped counts as passing, and it fails unless every dependency
result is `success`. A failed, skipped, cancelled, or absent dependency therefore fails it,
including a failed shared build or version inventory that skips a downstream matrix. Its name stays
the same when either matrix changes. It runs on pull requests, merge groups, and master pushes. The
master `release` job additionally requires the release build and version inventory before it
publishes artifacts.

The `master` ruleset (id `19534763`) requires this aggregate from the GitHub Actions app (id
`15368`). Audit the live protection without changing the ruleset:

1. Find the latest master push run and confirm its head is the merged commit:

   ```sh
   gh run list --repo qnox/krusty --workflow ci.yml --branch master --event push --limit 1 \
     --json databaseId,headSha,status,conclusion
   ```

2. Confirm that commit reported the aggregate as a successful GitHub Actions check (app id
   `15368`):

   ```sh
   gh api repos/qnox/krusty/commits/<headSha>/check-runs \
     --jq '.check_runs[] | select(.name == "ci-and-conformance") | {name, conclusion, app: .app.id}'
   ```

3. Read the current required checks:

   ```sh
   gh api repos/qnox/krusty/rulesets/19534763 \
     --jq '.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks'
   ```

   It must print `[{"context":"ci-and-conformance","integration_id":15368}]`. An open pull request
   must also list `ci-and-conformance` as required.

The JVM conformance, JVM byte-equality, and Native conformance badges change only when a master
`release` job publishes. To verify a publication, confirm that run's `release` job succeeded,
render its max-version reports locally, and compare the results with the three Gist files:

```sh
gh run view <run-id> --repo qnox/krusty --json jobs \
  --jq '.jobs[] | select(.name == "release") | {conclusion, steps: [.steps[] | {name, conclusion}]}'
v="$(just max-version)"
gh run download <run-id> --repo qnox/krusty -n "pct-$v" -n "jvm-byte-equality-$v" -n "native-pct-$v" -D target/reports
just conformance-badge target/reports/pct-$v/pct.txt target/reports/jvm-byte-equality-$v/jvm-byte-equality.txt target/reports/native-pct-$v/native-pct.txt
curl -fsSL https://gist.githubusercontent.com/qnox/dec8149bc4f43b203d6cc9adc14f2026/raw/krusty-conformance.json
curl -fsSL https://gist.githubusercontent.com/qnox/dec8149bc4f43b203d6cc9adc14f2026/raw/krusty-jvm-byte-equality.json
curl -fsSL https://gist.githubusercontent.com/qnox/dec8149bc4f43b203d6cc9adc14f2026/raw/krusty-native-conformance.json
```

The `label`, `message`, and `color` in `docs/badges/conformance.json` and
`docs/badges/jvm-byte-equality.json` and `docs/badges/native-conformance.json` must match the Gist's
respective files. A `release` job that skipped its publish steps left all three unchanged.

## Focused Runs

Pass normal Cargo test arguments through the harness:

```sh
./run-tests.sh --test conformance -- --nocapture
./run-tests.sh --test e2e lambda_e2e::lambdas_run -- --nocapture
```

Product e2e files are grouped into one `e2e` integration-test binary; external corpus/reference-toolchain suites are grouped into a separate `conformance` binary. Cargo compiles each
top-level `tests/*.rs` file as a separate crate, so grouping keeps link count and build artifacts
bounded. Focus a grouped test with a module/test-name filter (`lambda_e2e::lambdas_run`, a test function
name, or any normal libtest substring). The conformance suite remains available by `--test conformance` and is excluded from fast/coverage runs before it executes.

Any argument switches the harness to Cargo's normal focused runner with `--profile gate`. This is
useful for development, but use the no-argument harness for full-suite validation because it builds
once and schedules test binaries to preserve shared JVM runners.

## Byte-Identity Differential Mode

`KRUSTY_BYTE_DIFF=1` makes the box-conformance run ALSO compile every krusty-compiled corpus file
with the reference kotlinc (persistent in-process compiler server; results cached under
`target/cache/ref-classes/`, keyed by source + stem + classpath + per-unit code-generation modes +
dist identity + producing JDK) and compare the
two module-qualified class sets **byte-for-byte**. The diff reads the same reference inventory as
the byte score, so a run with both compiles each case once:

```sh
KRUSTY_BYTE_DIFF=1 KRUSTY_SERVER_POOL=4 ./run-tests.sh --test conformance -- --nocapture
```

The summary gains a `byte-diff: identical I | divergent D | ref-fail R` line, and a per-file report
(first difference per file) lands in `target/byte_diff_report.txt`. Every krusty-compiled topology,
including `// MODULE:` and mixed-Java tests, is reference-compiled the way the JVM byte-equality score
does it (see "Current Conformance"); kotlinc's `META-INF/*.kotlin_module` artifact is not compared.
`KRUSTY_BYTE_DIFF_DUMP=<dir>` also writes both class sets of each divergent case, one
`<module>/<internal name>.class` tree per compiler. The first run pays one warm
kotlinc compile (~0.4 s) per file — raise `KRUSTY_SERVER_POOL` on a large-RAM host; later runs hit
the on-disk cache. Pair with `KRUSTY_BOX_ONLY=<substring>` for a focused divergence loop.

To check that a change moves no output byte (a refactor, or a determinism fix), dump every compiled
class from two builds and compare the directories:

```text
KRUSTY_NO_RUN=1 KRUSTY_CLASS_DUMP=target/dump-before ./run-tests.sh --test conformance kotlin_codegen_box_conformance
KRUSTY_NO_RUN=1 KRUSTY_CLASS_DUMP=target/dump-after ./run-tests.sh --test conformance kotlin_codegen_box_conformance
diff -rq target/dump-before target/dump-after
```

Two dumps from the same build must also be identical: the compiler's output may not depend on hash
map iteration order.

## Profiling

For full-suite performance work, run:

```sh
./run-tests.sh
```

The final `SLOWEST TEST BINARIES` table is the first profiling signal. Use it before changing tests
or inventing custom loops. A nightly run also prints each test slower than 200ms:

```text
slow-test: summary count=<n> threshold=200ms
slow-test: <ms>ms bin=<invocation> test=<module>::<case>
```

`KRUSTY_SLOW_TEST_MS` changes the threshold. The duration is libtest's `--report-time`. A stable
compiler rejects that flag, so a stable run keeps the binary table and does not print per-test lines.
Coverage always builds with the pinned nightly, so its log includes the same lines.

For compiler-only conformance profiling, use:

```sh
KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1 ./run-tests.sh --test conformance kotlin_codegen_box_conformance -- --nocapture
```

This skips JVM execution in the conformance test, prints phase timing, and writes
`target/flamegraph.svg` plus a `top krusty frames` table on stderr. The whole run fits inside the
harness deadline (`KRUSTY_CONFORMANCE_TIMEOUT_SECONDS`, 120s): the harness symbolizes each sampled
instruction pointer once rather than once per stack, so turning ~80k samples into an SVG costs a
couple of seconds instead of the ~8 minutes `pprof`'s own `Report::build` takes on a full-corpus
profile. The SVG covers every sampled stack and runs to tens of megabytes — it is meant to be opened
in a browser, not read as text.

The e2e suite has its own built-in phase profiler: `KRUSTY_PROF=1` makes every harness helper print
`PROF\t<phase>\t<ms>` lines (`krusty` in-process compile, `kotlinc` reference compile incl. queue
wait, `box` JVM round-trip) to stderr — run the e2e binary with `--nocapture` and aggregate.

`just coverage` prints a wall-clock line for every phase of that job, in whole seconds:
`coverage: phase start <name>` when the phase begins and `coverage: phase <name> <seconds>s` when it
finishes. The phases are toolchain provision, instrumentation, one compiler build
(`build-compiler`) that emits the CLI, the language server, and their test binaries, each non-e2e
test binary, the e2e binary, and each coverage report. Cargo's own compile progress stays on stderr
for that build. The binaries and test harnesses are one cargo invocation, so the non-test library
is compiled once and the cfg(test) harness overlaps the binaries. Nightly rustc runs one frontend
job per core (`-Z threads`). The run ends with `coverage: phases` repeating every
finished phase and `coverage: phase total <seconds>s`, which is wall time from the first phase rather than
the sum of the rows. The conformance box runner prints the same shape as
`conformance-run: phase box-conformance <seconds>s`. The shared binary job prints
`shared-bins: phase conformance-test-binary`, `shared-bins: phase krusty-cli`, and
`shared-bins: phase krusty-build-tests`. Conformance lanes and Gradle lanes run those binaries. A
Gradle lane does not compile krusty.

Performance-relevant harness state:

- e2e dependency libs are compiled BY KRUSTY, in-process (`tests/common::compile_libs`), memoized
  per run — no reference-compiler round-trip and deliberately no on-disk cache: every run rebuilds
  its deps with the compiler under test. A lib krusty can't build fails the test with krusty's
  diagnostics; tests whose CONTRACT is consuming kotlinc-emitted metadata declare it with the
  explicit `*_ref` helpers (`compile_lib_ref`, `run_box_against_ref`, `Fixture::reference_lib`) —
  grep `_ref(` for the current emission/consumption gap inventory.
- The dependency-lib differential is ON BY DEFAULT: every krusty-built lib is also compiled with
  the reference kotlinc and the same `box()` result is asserted against both classpaths. Disable
  explicitly with `KRUSTY_LIB_CROSSCHECK=0` for a fast local loop. The assertion is BEHAVIORAL
  (same `box()` result), not byte-identity: lib classfiles still diverge from kotlinc's bytes
  (constant-pool ordering, `.kotlin_module` emission). `KRUSTY_LIB_BYTEDIFF_REPORT=1` (with
  `--nocapture`) prints a `LIBDIFF\t<identical|divergent|krusty-only|kotlinc-only>\t<entry>` line
  per lib entry — the convergence inventory for making byte equality the assertion.
- Persistent JVM pools (kotlinc compiler servers, JavaRunner) scale with the host: `ncpu/2` clamped
  to `[1, 6]`. `KRUSTY_SERVER_POOL=<n>` overrides in either direction (e.g. `1` on a swapping host).
- Directory classpath entries are shipped into the box runner's per-request classloader, so lib
  static state is fresh per `box()` call and runner JVMs are shared across tests.

Optional profiling knobs:

- `KRUSTY_TEST_TIMEOUT_SECONDS=<seconds>` overrides the 120-second deadline applied to every test
  binary except conformance and e2e; raise it explicitly on slow systems.
- `KRUSTY_CONFORMANCE_TIMEOUT_SECONDS=<seconds>` overrides the 120-second deadline for each
  full-suite or focused conformance pass and for the scored run (`conformance-run.sh`).
- `KRUSTY_E2E_TIMEOUT_SECONDS=<seconds>` overrides the 1800-second deadline for the single-process
  e2e suite, including a focused e2e run.
- `KRUSTY_NATIVE_CONFORMANCE_TIMEOUT_SECONDS=<seconds>` overrides the 600-second deadline for the
  single-process native codegen/box suite.
- `KRUSTY_TEST_JOBS=<n>` overrides full-suite test-binary parallelism.
- `KRUSTY_TEST_THREADS=<n>` overrides conformance worker threads.
- `KRUSTY_BOX_LIMIT=<n>` caps conformance corpus scanning for fast sampling.
- `KRUSTY_FAIL_CAP=<n>` caps reported conformance failures.
- `KRUSTY_BLESS_BOX_EXPECTATIONS=1` atomically rewrites the selected platform/version's fail and
  not-applicable inventories from a full local conformance run instead of checking against them.

Optional compiler trace:

- `KRUSTY_TRACE=resolve` prints selected classpath call-resolution decisions.
- `KRUSTY_TRACE=lower` prints IR lowerer bail reasons when the JVM backend skips a file.
- `KRUSTY_TRACE=all` enables every compiler trace category.

Trace output is disabled by default, reads the environment once, and does not format trace messages
unless the requested category is enabled.

## Current Conformance

There is no conformance number written down in this repository, deliberately. Every figure committed
to a document went stale within days of the commit that changed it, and a stale number read as
current is worse than no number. The live measures are three badges in `README.md`:

- The **conformance badge** (`krusty-conformance.json`) is the case pass rate: the share of the
  applicable cases whose `box()` returns `OK` on krusty-emitted bytecode, shown as
  `<pct>% (<passed>/<applicable>)`.
- The **JVM byte-equality badge** (`krusty-jvm-byte-equality.json`) is the share of matching leading
  bytes in module/name-paired `.class` files against the same-version kotlinc over those same cases,
  shown as
  `<pct>% (<matched>/<total> bytes)`.
- The **Native conformance badge** (`krusty-native-conformance.json`) is the pass rate of the
  independent Native lane: cases that compile, link, run, and return `OK` over cases applicable to
  Native, shown as `<pct>% (<passed>/<applicable>)`.

A case is applicable when kotlinc's own JVM box runner expects it to pass (`src/conformance.rs`
`backend_applicable`, mirroring `InTextDirectivesUtils.isPassingTarget`): cases restricted to other
backends by `TARGET_BACKEND`/`DONT_TARGET_EXACT_BACKEND`, or muted on JVM by the `IGNORE_BACKEND`
family, count in neither JVM badge's numerator nor denominator. Native applicability independently
uses the corresponding Native target/mute directives.

Both JVM compiles of a unit run under the same command line. `src/conformance/compiler_arguments.rs`
(`unit_kotlinc_arguments`) maps the case's configuration directives (`LANGUAGE`, `API_VERSION`,
`OPT_IN`, `EXPLICIT_API_MODE`, `ALLOW_KOTLIN_PACKAGE`, `JVM_TARGET`, `STRING_CONCAT`,
`ASSERTIONS_MODE`, `WHEN_EXPRESSIONS`, and the unit's own `LAMBDAS`, `SAM_CONVERSIONS`,
`JVM_DEFAULT_MODE`) to kotlinc arguments. The reference kotlinc receives them
verbatim; krusty's gate parses them with `krusty_cli::cli::parse`, the executable's own command
line, and compiles with the language settings and JVM backend that command line selects. Backend
applicability depends only on the corpus target and source universe. A targeted case whose selected
configuration krusty cannot compile remains applicable and fails with the refusal; it never moves
to the not-applicable inventory. `RETURN_VALUE_CHECKER_MODE` is a reference-oracle setup detail:
its `-Xreturn-value-checker` argument reaches only the reference compile
(`reference_only_kotlinc_arguments`) because the checker does not change runtime `box()` behavior.
Historical API levels which kotlinc's public command line cannot express fail closed until the
differential harness can install that typed configuration on both compilers.

Both lanes scan the same version-pinned corpus directory. The Native lane never inherits a JVM
mute: it excludes a case only for a Native/ANY target directive or a true target-runtime/source
requirement such as JVM classes. A missing harness capability is an expected failure and remains in
the denominator: this currently includes `// MODULE:` dependency construction, while Kotlin-only
`// FILE:` source sets and generated coroutine helpers run normally. The committed inventories keep
the Native denominator target-correct rather than hand-picked. These outcomes use the same generic
platform/version ratchet as JVM, so local and CI runs enforce identical state without a GHA cache.

For the byte score, each applicable case is compiled with krusty and also reference-compiled in the
same topology (Kotlin and Java sources, `// MODULE:` dependencies, directives, classpath, and JDK)
with the pinned kotlinc of that version. Both compilers' `.class` files, Java-emitted ones included,
are paired by module and JVM internal class name:

- A pair counts its common leading byte prefix as `matched` and the longer class length as `total`.
  Matching bytes after the first difference do not resume credit; exact equality is the only way to
  receive full credit.
- A class that only one compiler emits counts 0 out of its full length.
- A case whose `box()` does not return `OK` (krusty rejected it, emitted no `box()`, panicked, or the
  JVM returned anything else) counts 0 out of the summed size of kotlinc's classes.
- A reference compile or infrastructure failure fails the run; it is never a silent zero or a
  dropped case. A test directive the reference oracle cannot map to a kotlinc option (for example
  an unknown `// RETURN_VALUE_CHECKER_MODE:` value) is such a failure.

All reports keep integer counts: `passed` and `applicable` cases for each target's conformance, and
`matched` and `total` bytes for JVM byte equality. Each percentage,
`100 * count / of` rounded down to one decimal (`0.0` when the denominator is 0), is derived only
from its own sums. Thus only an exact count can display `100.0%`; `3145/3146` displays `99.9%`.
The score is never an average of per-case or per-shard percentages, and neither report's counts
stand in for another's. Only the box corpus feeds these badges. The non-box conformance tests
(serialization, KSP, and the rest of `just conformance-regressions`) are correctness gates in every
version lane and contribute neither cases nor bytes.

The scores and runtime correctness are separate gates. A case whose classes differ from kotlinc's but
whose `box()` returns `OK` still passes, and the exact outcome manifests (see "Box Outcome
Manifests") still decide whether the run succeeds. The run's stderr keeps both summaries:
the `cases: … | box()=OK: … | FAIL: …` line, and `byte-score: matched M | total T | P%  (N
applicable cases scored)`.

Commands:

```sh
just conformance [VERSION] [BYTES]      # stdout: "<pct> <passed> <applicable>"; default max version
just conformance-run "$(just conformance-bin)" <version> [BYTES]   # the same, for a prebuilt binary
just native-conformance-run "$(just conformance-bin)" <version> [NATIVE]
just wasm-conformance-run <wasm-js|wasm-wasi> "$(just conformance-bin)" <version> [REPORT]
just conformance-badge [CASES BYTES NATIVE]   # writes docs/badges/*.json previews
```

`scripts/conformance-run.sh` runs the full JVM corpus once. The process writes the
case report `<pct> <passed> <applicable>` to `KRUSTY_CONFORMANCE_REPORT` and the JVM byte report
`<pct> <matched> <total>` to `KRUSTY_JVM_BYTE_REPORT`; `KRUSTY_JVM_BYTE_REPORT` is also what turns
reference scoring on. The runner prints the case report on stdout, and the JVM
byte report on stderr (`conformance-run: Kotlin <version> JVM byte equality (matched/total .class
bytes): …`) and to the `BYTES` file when one is given. Either report missing or malformed, a
timeout, or a manifest mismatch fails the run; both reports are still written after a
manifest mismatch so the scores stay visible, and a run that stops earlier leaves `BYTES` empty.
`scripts/conformance-report.sh` owns the shared line format. A reference-cache miss compiles live
with kotlinc, so no recorded-byte setting is needed. The scored run prints a reference profile
after its timing line: reference-cache hits, cached rejections, and misses with the thread-summed
cache-read, compile, cleanup, and store time, then the kotlinc server's requests, starts, restarts,
pool wait, start time, and compile time. A cold run shows one miss per applicable case; a warm one
shows hits and no server requests.

`just conformance-badge` renders all three reports of the max version's target runs with
`scripts/conformance-badge.sh`. `docs/badges/conformance.json` gets the label
`Kotlin <version> conformance` and the message `<pct>% (<passed>/<applicable>)`;
`docs/badges/jvm-byte-equality.json` gets the label `Kotlin <version> JVM byte equality` and the
message `<pct>% (<matched>/<total> bytes)`; `docs/badges/native-conformance.json` gets the label
`Kotlin <version> Native conformance` and its independent `<pct>% (<passed>/<applicable>)` score.
Each is colored red below 10%, orange from 10%, yellow from 50%, and brightgreen from 70%. Pass all
three report paths together to render existing reports instead of running the suites:

```sh
v="$(just max-version)"
gh run download <run-id> -n "pct-$v" -n "jvm-byte-equality-$v" -n "native-pct-$v" -D target/reports
just conformance-badge target/reports/pct-$v/pct.txt target/reports/jvm-byte-equality-$v/jvm-byte-equality.txt target/reports/native-pct-$v/native-pct.txt
```

In CI, every target row prints and uploads its own report: JVM owns `pct-<version>` and
`jvm-byte-equality-<version>`, while Native owns `native-pct-<version>`. Only the
`release` job on master publishes the max version's three payloads to the badge Gist; pull requests
and merge groups never publish.

To inspect where bytes are lost, focus a case with `KRUSTY_BOX_ONLY=<substring>` (which turns the
score on for that selection, failed boxes included) and add `KRUSTY_BYTE_DIFF=1` for the first
difference per class set in `target/byte_diff_report.txt` (see "Byte-Identity Differential Mode").

Read the badges, or the `conformance` job of the latest master CI run, when you need today's
figures; use the target-specific recipes locally when you need this checkout's.

A count in a phase-log entry (`docs/IMPLEMENTATION_PLAN.md`) is a snapshot of what that phase
measured at the time it landed, not a claim about the present, and must not be quoted as current.

Two rules hold whatever the number is. Only compare `box()=OK` counts when `FAIL: 0` — a count taken
beside a failure is not a coverage measurement. And `KRUSTY_NO_RUN=1` is for compile/emit profiling
only: it skips JVM execution, so its output must never be reported as runtime conformance.

One historical artifact is worth keeping, because the shape of the graph invites the wrong reading:
the `1842 -> 1585` cliff in `target/ir_conformance_trend.csv` was a real temporary coverage drop from
a conformance-safety cleanup, not a regression. That cleanup stopped counting unsupported shapes as
compiled support (builder-inference directives, JS-runtime-only files, advanced `Result<T>`/value-class
cases, and unsupported `UByte`/`UShort` value-class paths). Later passes recovered past both plateaus.

For corpus triage, use the survey binary through the gate profile:

```sh
./run-tests.sh --survey
./run-tests.sh --survey --parse-only --report /tmp/krusty-parse.tsv
./run-tests.sh --survey --frontend-only --report /tmp/krusty-frontend-survey.tsv
./run-tests.sh --survey --frontend-only --file coroutines/example.kt
./run-tests.sh --survey --samples "inline splice failed"
```

The parse-only TSV records exact corpus file, Kotlin block, failure stage, line, column, diagnostic,
and source line. Its summary separately reports discovered cases, Kotlin blocks, parsed cases, lex
failures, parse failures, AST failures, and panics; any failed case makes the command fail. Backend
applicability is deliberately not consulted: valid syntax must reach a complete AST even where a
later phase has no support for it, and capability diagnostics come after parsing.

The parse gate does not reach a clean sweep of the corpus, and the one case it cannot is an input
defect rather than a parser gap: `contextParameters/withExtensionReceiverInType.kt` carries extra
closing parentheses, and the pinned reference compiler reports syntax errors at exactly the same two
positions krusty rejects. The file is marked backend-inapplicable, but applicability cannot make
invalid syntax valid. Accepting it would take a corpus-path exception, silent delimiter recovery
reported as success, or a weakened invalid-syntax diagnostic, so the gate keeps the defect visible
instead. No parser change should be needed once the pinned input is corrected.

The harness builds the survey with the normal `gate` profile, applies the configurable
`KRUSTY_TEST_TIMEOUT_SECONDS` deadline, provisions the same toolchain/corpus, and reports specific
inline splice bail callees when available. It covers the full corpus shape set the gate compiles:
single-file, `// FILE:`-split multi-file (with the generated `// WITH_COROUTINES` helpers), and
`// MODULE:` multi-module tests (each build unit compiled against its dependency modules' emitted
classes, `dependsOn` chains folded in — the splitting lives in `krusty::conformance`, shared with
the gate). Tests with `.java` sources are the one exception: they need the harness's persistent
javac runner, so the survey reports them under a dedicated `javac-dependent` category instead of a
first compiler error.

## JVM-Running Tests

Do not spawn `javac` or `java` per test unless the test is explicitly about the CLI/process boundary.
Use the shared helpers in `tests/common`:

- `compile_and_run_box`
- `run_box`
- `javac_run`

These helpers compile in process where possible and reuse persistent JVM runners/servers inside a test
binary. Per-test JVM startup is one of the easiest ways to degrade the suite.

## Native Runtime Drivers

`tests/native_runtime_e2e.rs` (in the `e2e` binary; filter `native_runtime`) builds each C driver under
`tests/native_runtime/` with the host clang against the freestanding runtime and runs it. A driver
succeeds with exactly `OK\n` on stdout and nothing on stderr (`run_driver`), or ends the way the
runtime ends a program with exactly the runtime's message (`run_driver_expecting_failure`).

The runtime's sources compile once per test-binary run, in parallel, to one object each
(`runtime_objects`); each driver then compiles its own `.c` with the same flags and links every
one of those objects, in the sources' sorted order. That is the program a single clang invocation
over all the sources would build, without recompiling about ten thousand lines of `-O2` C per
driver, which was nearly all of the drivers' time. The objects are not an archive, so a driver
still links the whole runtime and a duplicate definition still fails the link.

A driver whose expected answers are Kotlin's does not copy them into C. It prints a transcript — one
observation per line, runtime objects rendered through the runtime's own `toString` where that is the
claim — before its `OK`, and `run_driver_against_kotlin` compares it with the Kotlin program beside
it, `tests/native_runtime/<driver>.kt`. That program's `fun box(): String` builds the same lines;
the harness compiles it with the persistent reference kotlinc and runs it on the shared JVM
(`common::run_box`), requires it to succeed with whole lines, and fails on the first line where the
two transcripts differ, printing both. `tests/native_runtime/transcript.h` holds the C side
(`say`, `say_value`, `say_thrown`, …). Checks with no Kotlin counterpart — the pending exception's
identity, the exact calls into a program stand-in, failure messages — stay `CHECK`s in the driver, and
an answer the JVM cannot give within the box runner's 10-second limit stays pinned in the driver, with
kotlinc's answer recorded in the program beside the question.

kotlinc compiles every program beside a driver once per test-binary run, in one invocation for all
the programs that take the same kotlinc arguments (`kotlin_programs`), since a compile's fixed cost
outweighs a small program's. Each program is compiled in a package named after its driver, declared
at the start of the line after its file annotations so no line number moves, and its `box()` runs as
`<driver>.MainKt` from a class directory holding that program alone. Only a qualified name a program
prints could tell the package; name a class by `simpleName` instead. If kotlinc rejects a batch,
each of its programs compiles alone in its own test (`common::kotlinc_box_result`), which then fails
with kotlinc's diagnostics for that program exactly as before.

The native runtime's rule for where it answers differently from the JVM: it BEHAVES as Kotlin/Native
does — which exception type is thrown, a class's identity and names, what an `is` answers, iteration
order, a collection's semantics, the order of the calls it makes into the program — and SAYS what
the JVM says — an exception's message, a diagnostic's wording — wherever that is cheap, keeping
Kotlin/Native's message where the JVM's is not (a message describing a Java call, say). Every line
on which the two runtimes then answer differently is declared in the test, never left to a comment:
`run_driver_against_kotlin_with(driver, &[Divergence::native_behaviour(jvm, native), …])`, or
`Divergence::native_message` for a Kotlin/Native message kept, or `Divergence::not_yet_native` for a
known gap, a line the runtime answers as neither platform does because Kotlin/Native's answer needs
a call into the program the runtime cannot make yet (the declaration says what Kotlin/Native answers
and what closing the gap needs). kotlinc's program must answer exactly the declared `jvm` line and
the driver exactly the declared `native` line at the same place, so the oracle still checks the
JVM's side; every other line must be identical; and an undeclared difference fails, as does a
declared one that no longer occurs. No Kotlin/Native compiler runs here (the cached distribution
holds its stdlib, not its compiler), so each declaration cites where its native answer comes from: a
Kotlin/Native stdlib source (`JetBrains/kotlin` at the reference version's tag) or the disassembly
of the distribution's stdlib cache.

These drivers need the reference kotlinc like every other differential test: `just` provisions it, or
point `KRUSTY_KOTLINC` at a provisioned dist when running the filter by hand, for example from a
worktree:

```sh
KRUSTY_KOTLINC=/path/to/kotlinc/bin/kotlinc \
CARGO_TARGET_DIR=/path/to/main/target cargo test --profile gate --test e2e native_runtime
```

## Environment Overrides

The harness usually sets these itself through `just`. Override them only when testing a specific local
toolchain:

```sh
KRUSTY_KOTLINC=/path/to/kotlinc/bin/kotlinc \
KRUSTY_REF_JAVA_HOME=/path/to/jdk \
KRUSTY_KOTLIN_BOX_DIR=/path/to/compiler/testData/codegen/box \
KRUSTY_KOTLIN_STDLIB=/path/to/kotlin-stdlib.jar \
./run-tests.sh
```

A `KRUSTY_KOTLIN_BOX_DIR` override must be the `compiler/testData/codegen/box` directory of a
JetBrains checkout that also holds that tag's mock JDK.
