# Vendored Kotlin conformance cases

These `.kt` files are copied **verbatim** from the JetBrains/Kotlin compiler test suite,
`compiler/testData/codegen/box/`, and are licensed under **Apache License 2.0** (© JetBrains s.r.o.
and contributors). They are the canonical `fun box(): String → "OK"` conformance tests.

Only the cases that fall within krusty's currently supported language subset are vendored here, so
they run in normal `cargo test` (see `tests/box_vendored_e2e.rs`). The **full** suite (10,009 cases)
is run directly against the provisioned Kotlin box corpus by `tests/kotlin_box_conformance.rs`, which
skips everything krusty can't yet compile and asserts krusty never miscompiles a case it accepts.

To refresh / widen this set, provision the Kotlin box corpus and copy over any newly-passing cases.
