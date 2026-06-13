# krust — implementation plan

Phased, each phase ends in a **green `cargo test`** and a runnable artifact. The pipeline is built
front-to-back so the streaming/arena shape is real from the start, then widened.

Legend: ✅ done · 🚧 in progress · ⬜ todo

## Phase 0 — Foundations  ✅
- ✅ Cargo project (lib + bin), local `cargo test`/`cargo run`. Toolchain: rustc 1.96 + gcc linker.
- ✅ `token.rs`: token kinds, `Span { lo:u32, hi:u32 }`, keyword table (types are idents, not kw).
- ✅ `lexer.rs`: byte-slice → `Vec<Token>`; idents, keywords, int/long/double/string/bool literals,
  multi-char operators, line+block comments, newline-as-token layout. 6 unit tests.
- ✅ `diag.rs`: `Diagnostic`, `DiagSink`, line/col rendering. 2 unit tests.
- ✅ **Exit met:** 8 tests green; driver lexes the real `multifile`/`bodyheavy` bench files
  (5254 tokens/file, 0 errors).

## Phase 1 — Parse to arena AST  ✅
- ✅ `ast.rs`: index-based arena (`ExprId/StmtId/DeclId` = `u32` into parallel `Vec`s; no Box/Rc
  graph, bulk-freeable). Decls (`fun`), stmts (`local/assign/return/while/expr`), exprs
  (literals/name/unary/binary/member/call/if/block). S-expr `debug_tree` for tests.
- ✅ `parser.rs`: recursive descent for decls/stmts; **Pratt** for expressions with the Kotlin
  precedence table (`|| < && < eq < cmp < add < mul < prefix < postfix`). Newline = terminator.
- ✅ Tests: 10 parser tests (precedence, assoc, paren, member-call, unary, if, block/while, package).
- ✅ **Exit met:** all `tests/cases/*.kt` + the in-subset bench files parse (multifile×20,
  many_functions = 500 decls). 18 tests green total.
- Note: `bodyheavy` uses `xor` (infix function) + `;` — **out of v0 subset**; not a krust target.

## Phase 2 — Types & resolution  ⬜
- `types.rs`: `TypeId` + primitive table (Int/Long/Double/Boolean/String/Unit), join (common
  supertype) for `if`.
- `resolve.rs`:
  - Stage C: collect top-level signatures → global `SymbolTable` (cheap, no bodies).
  - Stage D: per-file typecheck — locals scope stack, name resolution, expr typing, `val`-reassign
    error, arithmetic/concat typing rules, `if`/`while`/`return` checks.
- Diagnostics for type errors. Tests assert types of expressions + expected errors.
- **Exit:** typecheck passes/fails correctly on the §7 edge cases.

## Phase 3 — JVM class-file writer  ⬜
- `codegen/classfile.rs`: constant pool (Utf8/Class/NameAndType/Methodref/Fieldref/String/
  Integer/Long/Double), method + `Code` attribute, stack-map-frame **computation** (or target
  v50/`-noverify` first, then add frames for v51+). Emits a `FileKt`-style class with `public static`
  methods.
- Verify: `java -Xverify:all` loads emitted classes.
- **Exit:** emit a hand-built `add(II)I` class that verifies and runs.

## Phase 4 — Lower + emit the subset  ⬜
- `ir.rs`: minimal stack IR (or direct AST→bytecode for v0).
- `codegen/emit.rs`: literals, arithmetic (with Int 32-bit / Long / Double opcodes), comparisons &
  branches, boolean short-circuit, string concat (v0: `StringBuilder` sequence — simplest verifiable
  form; document that kotlinc uses `invokedynamic`), local slots, `if`/`while`, calls, `return`.
- `driver.rs`: the **streaming loop** with explicit per-file arena drop + a `--emit-stats` flag
  printing peak/working-set.
- **Exit:** `cargo run -- tests/cases/arith.kt` produces a verifying, running `.class`.

## Phase 5 — Differential harness vs kotlinc  ⬜
- `harness/`: (a) locate reference kotlinc (wrap the `kotlin-compiler` 2.4.0 jar in `~/.m2` via a
  tiny launcher, or system `kotlinc`); (b) for each case: compile with both, run a generated
  `Main` calling the functions with fixed inputs, compare stdout/exit; (c) `javap -c -p` normalized
  structural diff; (d) verifier gate.
- Wire as `cargo test --test diff` (integration test shelling out).
- **Exit:** edge-case suite green (execution-equivalent to kotlinc).

## Phase 6 — Real interop + scale  ⬜
- `.class` signature reader (replace the hardcoded JDK table) so arbitrary JDK calls resolve.
- Minimal Java *source* front end (signatures only) for mixed kt+java compilation.
- Benchmark peak RSS vs kotlinc on `many_functions`/`multifile`/`bodyheavy`; confirm ~constant in
  file count. Add `invokedynamic` string concat to match kotlinc structurally.

## Phase 7 — Hardening  ⬜
- Fuzz the lexer/parser; property tests for arithmetic semantics vs a reference evaluator.
- Expand the subset opportunistically (when/nullable) only if it serves the memory thesis.

---

### Working agreements
- Every phase: `cargo test` green before moving on; no `unwrap` on user-input paths in the driver.
- Keep the AST/IR **index-based** (no `Box`/`Rc` graphs) — that's the experiment.
- Record every Kotlin-semantics decision (overflow, division, concat order) in SPEC §7 with a test.
- The harness is the source of truth for "correct"; don't claim a feature works without a diff test.
