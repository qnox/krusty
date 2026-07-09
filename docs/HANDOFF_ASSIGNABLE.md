# Handoff — GSig→Ty + unified assignability (branch `metadata-primary-generic-signatures`)

## State
- **Uncommitted** working tree (HEAD `fa40a76`). All work below is working-tree only.
- Gate: **box conformance FAIL:0** (1685 pass). Full suite: **12 pre-existing e2e fails** (NOT regressions).
- Branch **diverged from origin: 62 ahead / 11 behind** → push needs `git rebase origin/metadata-primary-generic-signatures` first (non-ff).
- Run gate: `./run-tests.sh` (self-provisions). Box only: `./run-tests.sh --test conformance` (target is `conformance`, NOT `kotlin_box_ir_jvm_conformance`). Never pipe gate through `tail` — masks exit code. Never bypass hooks.

## The arc (evolved goal)
ir_lower = pure consumer (DONE earlier) → one federated resolution via SymbolResolver → **kill GSig, use Ty** (DONE) → **one `is_assignable` over full Ty, replace the subtype scatter** (IN PROGRESS).

## Landed this session (all green modulo the 12 pre-existing)
1. **GSig killed → Ty.** `Ty::TyParam(name,bound)` already existed. Removed `enum GSig`, `gsig_to_ty`, `gsig_tys`. `GenericSig{receiver:Option<Ty>,params:Vec<Ty>,ret:Ty}` + `PropertyInfo{receiver:Option<Ty>,ty:Ty}` carry `Ty`. Decoders produce `Ty` directly: `metadata.rs::parse_type_gsig`/`gsig_from_kotlin_class`, `jvm_libraries.rs::parse_gsig`. `unify_gsig`→`unify_ty(sig:Ty,..)`, `gsig_to_ty`→`ty_subst(sig:Ty,binds)` (unbound TyParam erases to Any). Binds keyed by formal NAME.
   - Regression fixed: scoped ext builder (`jvm_libraries.rs ~1701`) dropped `signature`; reified splice reads formals via `signature_formals(c.signature)`. Fix = `signature: cand.as_ref().and_then(|c| c.signature.clone())`. (`build775_ee1`.)
2. **average/reduction element fix.** `source_receiver_rank` erased receiver element. Added `receiver_type_args_match` (covariant type-arg check). `averageOfDouble`/`sumOfInt` now correct. No scalar hack (user rejected).
3. **NEW `src/assignable.rs`** — the ONE relation. `is_assignable(cx,oracle,sub,sup)` (+value-class erasure) and `is_subtype(...)` (pure). Full Ty lattice: primitives (scalar TARGET no widening; scalar SOURCE boxes), Obj+covariant args (Any/TyParam=wildcard), Fun (contravariant params/covariant ret), Array covariant, Nullable, Nothing⊑all, null⊑nullable, TyParam via `TyCtx` bound. `trait TypeOracle{direct_supertypes,value_underlying,canonical_class}`; adapters `SourceOracle`/`PlatformOracle` in symbol_resolver. **9 unit tests green** (`cargo test --lib assignable`).
   - Wired (gate-verified behavior-preserving): `receiver_type_args_match`→is_assignable (+wildcard guard: metadata drops nullability so `T?` receiver-elem reads bare Any); `is_classpath_subtype`→is_subtype (routes `arg_subtype_assignable`/`ref_subtype_fits`/`descriptor_arg_subtype_of_param` transitively).

## Remaining — "replace all the rest" (task #6)
Still on old helpers; each needs its own gate run:
- `reference_subtype` (symbol_resolver, self.src walk) → is_subtype via SourceOracle. **Edge: `is_subtype(Int?,Number)`=false vs old true** (nullable-arg-to-nonnull); verify no overload fallout.
- `obj_is_subtype` (resolve.rs, checker) — MANY callers (when-exhaustive, casts, smart-cast, array covariance). Biggest/riskiest.
- `array_covariant_assignable`/`elem_covariant_assignable` (resolve.rs) — subsumed by is_subtype (array covariance already in it).
- `arg_assignable_simple` + `arg_fits` — LOOSE applicability ranking (numeric→numeric permissive), NOT subtyping. Likely stays separate or thin layer over is_assignable.

## The 12 pre-existing e2e fails (backlog, block `just test` → block push)
`primitive_spread`×3, `ir_edge_coverage`×2, `build702_fq_trailing_lambda`×2, `named_args_classpath`, `js_backend_coverage::is_string_typeof`, `feature_box`, `feature_coverage_n::run_catching_get_or_else`, `build840_jj1::vararg_modifier_still_parsed`.

### primitive_spread ROOT-CAUSED (not yet fixed)
`f(vararg xs: Int)=xs.sum()`; `xs.sum()` on `IntArray` resolves to bogus `sumOfInt([Ljava/lang/Integer;)I` instead of `sum([I)I` → VerifyError. Two builders make `sum` candidates for an array receiver: scoped (`1592`, correct `sum([I)`) and metadata-mangled `@JvmName` path (`~1985`, produces the boxed `sumOfInt` — the private `sumOf{selector}` HOF's `@JvmName=sumOfInt` + `T[]` boxed receiver leaks into a plain `sum` query). **Fix #1 (targeted):** in the mangled builder, drop the `@JvmName`-`Of` candidate when the receiver is an array (`receiver.array_elem().is_some()`) — Of-renaming is Iterable-only. (My earlier `elem_mangled` guard at 1615 hit the WRONG builder — reverted.) **Fix #2 (root, big):** model `IntArray`=`Obj("kotlin/IntArray")`, `Array<T>`=`Obj("kotlin/Array",[T])`, delete `Ty::Array` special-casing so one machinery handles arrays — the user's preferred "eliminate the separate branches".

## Gotchas
- `Ty::Array(elem)` has intrinsic element: `type_args()`==`[]` (not `[elem]`), so arrays bypass all generic-Obj machinery → parallel branches everywhere. This duality is the source of the array-reduction bug.
- No AI attribution in commits (hard rule). Commit only when user asks.
- Memory: `assignable-unification.md`, `resolver-unification.md`.
