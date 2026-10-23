# KLIB inline-body migration (#2459)

The maintainer explicitly approved landing #2459 with its remaining Native/Wasm conformance
regressions on 2026-10-22, then recovering them through small follow-up PRs. The recorded failures
are accepted migration debt, not working features. JVM, lint, core tests, exact KLIB contracts and
the existing fatal-outcome protections remain required.

## One-time outcome reset

Full local runs of the frozen `56efc54cf73a6a7a5795ff21df3a5735f09a97e0` compiler record each
supported exact Kotlin release and each Native, wasm-js and wasm-wasi lane independently. The
supported releases come from `kotlin-versions`; missing release files retain ordinary predecessor
inheritance. Neither applicability inventories nor JVM/CLI outcomes are part of this exception.
The conformance binary SHA-256 is
`c63ad574e23f06eaf176992f9d09045766bd8714e2f6b1717f0c56cadbf147c5`.

`scripts/outcome-list-migrations.tsv` pins each effective release's exact loaded old/new manifest
blob identities. The outcome guard accepts only a matching row newly introduced since the
comparison merge-base. One extra failure, a different target/release/blob, an uncommitted row, or
replaying a historical row still fails. Subsequent work resumes the normal shrink-only rule.
Historical migration rows are append-only. Once the comparison merge-base or target revision
records approval #2459, neither replaying a row nor appending a different blob transition can
renew that spent approval, including from a branch forked before the migration.
The box runner, panic policy and owned-fixture setup checks are unchanged; no test is removed or
reclassified as inapplicable. Old timed-out runs and unexecuted WASI rows are not reset evidence.
The complete Native corpus has an explicit 1800-second CI budget after two earlier rows exhausted
600 seconds before producing a complete inventory. Ordinary/local/focused defaults are unchanged;
a timeout still fails rather than becoming an expected outcome.

## Recorded checkpoint evidence

All twelve full checkpoint rows completed with zero compiler panics and zero fatal outcomes.
The ratios below are passed/applicable, not claims that the recorded failures work. Applicability
entry sets are unchanged; manifests are canonicalized in ascending supported-release order using
the ordinary recorder, with every effective set checked afterward.

| Exact Kotlin release | Native | wasm-js | wasm-wasi |
| --- | ---: | ---: | ---: |
| 2.4.0 | 4254/7022 | 716/7070 | 716/7074 |
| 2.4.10 | 4255/7023 | 716/7071 | 716/7075 |
| 2.4.20 | 4316/7131 | 723/7203 | 722/7203 |
| 2.4.21 | 4316/7132 | 723/7204 | 722/7204 |

Against the master snapshot integrated for this landing, Native .0/.10 each add 1,270 failures
and recover 131; .20/.21 each add 1,275 and recover 133. Each Wasm row adds 19 failures, recovering
51 on .0/.10 and 52 on .20/.21. These are independent full-run sets, not inferred host results.
Fresh integrated-head CI remains required; these receipts retain the frozen checkpoint identity
above and are not relabeled as a run of the subsequent master merge.

## Small follow-ups

- #2520: explicit dependency type evidence and complete formal bounds; work is already reserved
  on `fix/dependency-body-type-evidence`, outside the landing snapshot.
- #2521: serialized lambda capture/parameter frames; start with immutable captures.
- #2522: exact dependency backing-field accessors and generic receiver substitution.
- #2523: dependency member/default-interface dispatch, separate from module-interface tables.
- #2524: dependency singleton initialization; ThreadLocal storage follows separately.
- #2525: declared generic numeric carriers on both Wasm hosts; nullable/value-class/function
  carriers and array covariance remain separate changes.

Each follow-up establishes the JVM/reference result first, validates the affected runnable targets,
and removes only demonstrated recovered outcome entries. Whole-corpus first-blocker frequencies
are not promised recovery counts. Continuation interception additionally needs real field, virtual
dispatch and suspend-frame support; it is not replaced with a name-based algorithm adapter.
