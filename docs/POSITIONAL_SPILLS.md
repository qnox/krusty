# Positional continuation spills (kotlinc parity) — design

The last mission-core RED_ABI family (10 files) is continuation **field-count** divergence.
Decoded from `IdentityLinkService.link` bytecode (kotlinc 2.4):

## kotlinc's model

- At **each suspension**, kotlinc stores the lexically **in-scope named variables** — value
  parameters first (always in scope), then locals in declaration order, block-scoped locals only
  while their block is open — into `L$0..L$k` **by position within that suspension's list**
  (per-kind counters: references `L$`, ints `I$`, longs `J$`, …).
- Each **resume arm** restores exactly its own suspension's list (getfield → checkcast → store).
  There is no loop-top restore-all.
- Field count = **max in-scope count over suspensions** (per kind). Different variables REUSE the
  same `L$N` in different states: `link()` s1 = 4 params → `L$0..3`; s2 = params + `existing` +
  `updated` → `L$4`,`L$5`; s3 = params + `existing` + `identityLink` → `L$4`,`L$5` again.
  kotlinc: 6 fields; krusty's one-field-per-variable union: 7 → RED_ABI.

## krusty refactor plan (src/jvm/suspend.rs, both machines: named fn ~1300-1620, lambda ~1700-1900)

1. **Pre-flatten scope snapshot**: after hoisting + catch-spill allocation, one lexical walk of the
   final body computing `HashMap<ExprId /*suspend-call expr*/, Vec<(u32, Ty)>>` — params prefix +
   in-scope eligible locals (the existing spilled-union filtered set) in declaration order. Walk
   descends Block/When/While/Try statement lists with scope push/truncate; a suspend-call node
   snapshots the current scope. `IrCatch.var` remapped via `catch_spills` (cvar → ev) is in scope
   inside its catch body.
2. **Flat**: keep `spilled` (union) for `is_spilled`; add `scopes` (the map), `state_scope:
   Vec<Option<Vec<(u32,Ty)>>>`, `cur_state`. `emit_call(call, resume)`: replace `spill_all` with
   positional stores of `scopes[&call]` (per-kind running position → field via layout map);
   record `state_scope[resume] = Some(list)`.
3. **goto**: drop `spill_all` — all transfers go through the label dispatch inside one invocation,
   locals persist; only resume arms restore. Debug-assert a goto never targets a state with
   `state_scope Some` (would read stale fields).
4. **Assembly**: delete the loop-top restore block; per **resume arm** k prepend restores of
   `state_scope[k]` (params via `SetValue`, locals via `Variable` decl — same rules as the old
   loop-top block). Field layout: `result`, `label`, then per-kind maxima (`L$0..`, `I$0..`, …)
   from the max list sizes; `build_continuation_class`/lambda-fields take the layout, and
   `param_caps` ctor stores use the params' fixed leading positions (params are a stable prefix of
   every list).
5. **bind_from_r**: the binding local of the JUST-completed suspension is NOT in the arm's restore
   list — declare it (`Variable`) unless it IS in the list (conditional-init merge case) → then
   `SetValue`. Decide via `state_scope[cur_state]`.
6. `assigned` definite-assignment gating becomes unnecessary for stores (in-scope ⇒ initialized);
   keep for any other consumer until proven removable.

## Verification loop

- Repro: `IdentityLinkService.kt` → `$link$1` must have exactly `L$0..L$5` (see
  `/tmp/abidiff.py` in the infragnite worktree, or javap field diff).
- `./run-tests.sh --test conformance kotlin_codegen_box_conformance -- --test-threads=1`
  (JAVA_HOME required) after every stage — the corpus is the never-miscompile detector.
- Gate: `./gradlew :mission:mission-core:krustyVerify -Pkrusty.binary=…` in the infragnite
  worktree; target 96 → 106 GREEN.
