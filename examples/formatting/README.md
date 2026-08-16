# Formatting examples

Standalone, intentionally unformatted Kotlin files for trying out krusty-lsp's
ktlint-compatible `textDocument/formatting` by hand: open any of them in an editor wired
to krusty-lsp and run "Format Document". The `.editorconfig` here (4-space indent) is
picked up from the file's directory, exactly as ktlint would.

- `basics.kt` — spacing, redundant semicolons, brace placement, expression bodies, import
  ordering.
- `signatures.kt` — multi-parameter signature wrapping with trailing commas, type-parameter
  bounds, expression-body `when`.
- `enums.kt` — enum entries: inline when memberless and single-line, one-per-line with a
  trailing comma otherwise.
- `strings_and_when.kt` — raw strings, `.trimIndent()`, single-line `when` expansion.
- `continuations.kt` — operator continuations, multiline operands, argument-list
  wrapping, object arguments, and glued lambda arguments.
- `when_bracing.kt` — once any `when` entry has a block or separate-line body, bare
  bodies get braced and entries are blank-separated; ambiguous multi-statement bodies
  stay untouched.

The expected formatter output for each file is byte-identical to `ktlint --format`
(ktlint 1.8.0, ktlint_official style), which is what the fixture corpus under
`crates/krusty-lsp/tests/fixtures/formatting/` pins down.
