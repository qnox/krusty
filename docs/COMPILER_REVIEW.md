# Compiler architecture review

This review focuses on why adding individual Kotlin features has not translated into broad
conformance coverage. The recurring pattern is not lack of feature code. It is duplicated resolution
paths, JVM representation leaking above the backend boundary, and per-feature side tables that make
each new case add branches instead of removing them.

## Architecture baseline

`docs/ARCHITECTURE.md` says:

- front-end modules must not depend on backend details;
- types and IR should be Kotlin-semantic;
- descriptors, internal JVM names, primitive boxing, value-class erasure, `$default`, `INSTANCE`, and
  inline bytecode details belong at the JVM backend boundary;
- libraries should be queried through a target-neutral symbol interface.

The current tree has moved in that direction with `SymbolSource`, `ModuleSymbols`,
`LibrarySet::functions`, `FunctionInfo`, `CallSig`, `FnFlags`, and `InlineKind`. That is the right
axis. The problem is that the older paths still exist and are actively used.

## Findings

### 1. `Ty` still carries some runtime-shape facts, but JVM descriptors have moved out

Evidence:

- `Ty::descriptor()` has been deleted; JVM descriptor construction now lives under `jvm::names`.
- `src/types.rs` still carries internal class-name strings in `Ty::Obj`, and some non-JVM modules still
  reason about runtime-shape facts such as boxed primitives, reference-like nullable scalars, and
  specializable scalar bounds.
- Kotlin classes and Java runtime classes are still mixed in some `Ty::Obj` values: examples include
  `kotlin/String`, `java/lang/StringBuilder`, `java/lang/Class`, and `kotlin/jvm/functions/FunctionN`.

Why this hurts coverage:

The checker must know when `kotlin/String` and `java/lang/String` are "the same enough", when
`UInt` is an `Int`, when nullable primitives box, and when a function type is `FunctionN`. Every one
of those backend decisions leaks into overload keys, subtype checks, hardcoded branches, and
metadata recovery.

Direction:

Continue moving runtime-shape decisions out of `Ty` and common lowering into target-owned helpers.
Replace `Ty::Obj(&str, args)` with a Kotlin class id and let the JVM backend map that id to an internal
name. Keep temporary compatibility adapters, but require new code to use Kotlin ids.

Expected deletion:

- ad hoc string checks for `java/lang/*`, `kotlin/jvm/functions/*`, and descriptor fragments in
  non-JVM modules;
- several erasure helper branches in `resolve.rs`.

### 2. Resolution has two competing APIs

Evidence:

- New generic API: `SymbolSource::functions`, `SymbolSource::resolve_type`, `FunctionSet`,
  `FunctionInfo`, `CallSig`, `LibraryType`.
- Old specialized methods in `LibrarySet`: deleted. The provider boundary is now `SymbolSource`.
- Package-level and member callable selection now goes through `CallResolver`; remaining architecture
  work is concentrated in JVM-detail leakage in checker/lowerer and private construction helpers.

Why this hurts coverage:

A call can be resolved through the new `functions()` path or through one of the old methods. That
means metadata, default arguments, lambda receiver types, inline flags, return nullability, and
physical return types are not guaranteed to travel together. New features naturally bolt onto the
path that happens to make a test pass.

Direction:

Make `FunctionSet` the only call-resolution entry point. Put every fact needed by checker and lowerer
on `FunctionInfo` or a nested resolved-call handle:

- logical params and return;
- physical params and return;
- source call shape: names, defaults, vararg, lambda param types, receiver-lambda marker;
- callable origin and emit handle;
- inline requirement;
- suspend flag;
- receiver rank and member/extension/top-level kind.

Then delete the remaining member/builtin side channels and metadata follow-up methods in stages.

Expected deletion:

- direct package-call compatibility shims in `LibrarySet`;
- metadata probe methods that project a single fact after a callable was already chosen;
- duplicate overload matching code in `JvmLibraries`.

### 3. `TypeInfo` is a pile of side channels

Evidence:

`TypeInfo` contains many expression-keyed or statement-keyed maps:

- `ext_calls`;
- `expr_lowers`;
- `stmt_lowers`;
- `local_funs`;
- lowering-local shared-cell state.

Why this hurts coverage:

These maps are parallel facts about resolution. Lowering must remember to check them in the right
order before falling back to AST shape. Each new Kotlin construct adds another side table and another
precondition. This is why code grows by cases.

Direction:

Introduce one resolved-expression table:

```rust
pub enum ExprResolution {
    Value(ValueRef),
    Call(ResolvedCall),
    Property(ResolvedProperty),
    Constructor(ResolvedConstructor),
    Lambda(LambdaContext),
    Operator(ResolvedOperator),
}
```

`ResolvedCall` should contain the selected `FunctionInfo` plus argument mapping. `ResolvedProperty`
should contain getter/setter handles. `LambdaContext` should contain parameter types, receiver type,
inline/capture policy, and splice policy.

Expected deletion:

- most expression-keyed maps in `TypeInfo`;
- special-case probes in `ir_lower.rs` before ordinary call lowering;
- repeated reconstruction of argument maps and callable descriptors during lowering.

### 4. IR is partly backend-neutral and partly JVM bytecode IR

Evidence:

- `src/ir.rs` claims no JVM descriptors in IR, but `Callee::Static`, `Virtual`, and `Special` carry
  `owner`, `name`, and `descriptor`.
- `IrExpr::ClassConst`, `NewExternal`, `ExternalStaticField`, and comments around `INSTANCE`,
  `$default`, and `PropertyReference*Impl` are JVM-specific.
- `ir_lower.rs` builds descriptors and JVM owners before the backend gets control.

Why this hurts coverage:

Once lowering commits to a JVM descriptor, generic Kotlin facts are lost. Later passes must re-infer
logical return types, nullability, receiver function types, and value-class boxing from side tables or
string parsing. That drives the growing branch count.

Direction:

Split IR into:

- common Kotlin IR: semantic callee ids, class ids, function ids, property ids, argument mapping,
  logical types;
- JVM-lowered IR: descriptors, owners, `$default`, `INSTANCE`, bridge methods, coroutine CPS shape,
  value-class erasure.

The existing `Callee` variants can become a backend handle enum only after JVM lowering, not in the
common IR produced by `ir_lower.rs`.

Expected deletion:

- JVM descriptor fields from common `Callee`;
- descriptor formatting in `ir_lower.rs`;
- backend-specific call forms from source lowering.

### 5. Hardcoded builtin and stdlib behavior remains in the checker

Evidence examples:

- `resolve.rs` has name tables for conversions (`toInt`, `toLong`, etc.), string/StringBuilder
  methods, range classes, enum `values`/`valueOf`, `Unit`, `field`, `class`, primitive companions,
  scope functions, array factories, bitwise methods, primitive operator methods, and collection
  property remaps.
- Some comments say "no hardcoded names", but nearby code still matches names or JVM owners.

Not all hardcoding is equally bad. Syntax-level operators and language-reserved names belong in the
compiler. Library facts do not.

Direction:

Classify every hardcode into one of three buckets:

1. Language syntax: keep in front end. Examples: `this`, `super`, `field`, `class`, `Unit`, primitive
   binary operators, control flow.
2. Kotlin builtins metadata: route through `LibraryType` or builtins declarations. Examples:
   `String.length`, `Char.code`, `List.get`, ranges, collection property remaps.
3. JVM realization: move to JVM lowering. Examples: `StringBuilder`, `INSTANCE`, `Companion`,
   `$default`, method descriptors, function interface internal names.

Expected deletion:

- string/StringBuilder method tables from `resolve.rs`;
- range member hardcodes after range types are represented as builtins/operators;
- duplicated enum and companion lowering rules once properties/constructors have unified handles.

### 6. Metadata is decoded, but not consistently authoritative

Evidence:

- `jvm/metadata.rs` decodes return class, nullability, receiver types, default params, param names,
  receiver function type annotations, inline and suspend flags.
- `jvm/classpath.rs` caches these facts through `MetaFnsCache`.
- `jvm/jvm_libraries.rs` sometimes folds these facts into `FunctionInfo`, but also still calls
  metadata probes while constructing or resolving calls.
- `resolve.rs` and `ir_lower.rs` still have fallback paths that do not necessarily carry the metadata
  facts selected for the overload.

Why this hurts coverage:

Kotlin metadata is the semantic source for exactly the cases that conformance stresses: generic
returns, nullable returns, receiver lambdas, default arguments, type aliases, suspend logical return,
and `@JvmName` overloads. If metadata is consulted after overload selection or through a name-only
map, overload-specific facts are easy to lose.

Direction:

Decode metadata once into `MetaFn`, convert each overload into `FunctionInfo`, and never expose
name-only metadata maps to the checker. The selected overload must carry the metadata facts that made
it selectable.

Expected deletion:

- `metadata_return_type`, `metadata_return_nullable`,
  `metadata_receiver_types`, `metadata_param_names`, `metadata_param_defaults`,
  `metadata_kept_params`, and similar public probes from non-construction paths;
- name-only maps where overload-specific data is required.

### 7. Data structures are split by feature instead of by compiler concept

High-value combinations:

- `Signature`, `LibraryCallable`, and `FunctionInfo` overlap. Introduce a single
  semantic `CallableSig` plus an optional `BackendCallable`.
- `ClassSig` and `LibraryType` overlap. Introduce a common `TypeDecl`/`TypeShape` and implement both
  source and classpath providers through it.
- `CallSig`, `Signature.param_*`, and metadata param projections overlap. Keep one `ParamShape` list:
  `{ name, ty, default, vararg, lambda_shape }`.
- `InlineKind` is already a good consolidation. Continue that style for visibility, callable origin,
  type kind, and emit form.
- `ClassNames` and `LibrarySeed` should carry class ids, not strings that are sometimes Kotlin names
  and sometimes JVM internal names.

Expected deletion:

- conversion glue between `Signature` and `FunctionInfo`;
- duplicated param vectors and parallel metadata vectors;
- repeated `HashMap<String, Signature>` plus `Vec<Signature>` overload handling.

## Refactor order

### Step 1: Make `FunctionSet` the only classpath/module call query

Do this before touching `Ty`. It is the highest deletion-per-risk step because the generic surface
already exists.

Work:

- Add an overload selector that consumes `FunctionSet`, argument types, type args, and named args.
- Return a `ResolvedCall` carrying `FunctionInfo` plus argument mapping.
- Replace direct package-call compatibility calls in `resolve.rs` first, then in `ir_lower.rs`.
- Delete the old `LibrarySet::resolve_callable` wrapper once tests use `CallResolver` directly.

Success check:

- `grep -R "resolve_callable" src tests --exclude-dir=target` returns nothing.
- No metadata probe is called after a call has already been selected.

### Step 2: Replace `TypeInfo` side maps with `ExprResolution`

Work:

- Add `expr_resolutions: Vec<Option<ExprResolution>>` parallel to `expr_types`.
- Migrate one call kind at a time: extension calls, companion calls, receiver lambdas, plus-assign.
- Make lowering consume only `ExprResolution` for those cases.

Success check:

- Side maps shrink with every migration.
- `ir_lower.rs` has one call-lowering entry that switches on `ResolvedCall.kind`, not on many maps.

### Step 3: Split common IR from JVM-lowered IR

Work:

- Introduce semantic call/property/constructor ids in common IR.
- Move `owner/name/descriptor`, `$default`, `INSTANCE`, and `invoke*` choice to a JVM lowering pass.
- Keep current JVM callee handle as the output of that pass.

Success check:

- `src/ir.rs` has no JVM descriptors.
- `src/ir_lower.rs` does not format descriptors.

### Step 4: Move ABI formatting out of `Ty`

Work:

- Add `jvm::abi::descriptor(Ty, TypeEnv)` or a `TargetAbi` trait.
- Change non-backend collision keys to semantic erased keys.
- Move class-name mapping into backend ABI code.

Success check:

- `Ty::descriptor()` is only used in `src/jvm/**` or is deleted.
- `types.rs` does not import `crate::jvm`.

### Step 5: Classify and delete builtin hardcodes

Work:

- Create a small manifest listing allowed front-end language names.
- Every other builtin/library branch must route through `LibraryType`, `FunctionSet`, or backend
  lowering.
- Add a test that fails when new non-JVM files introduce disallowed JVM strings.

Success check:

- Hardcode grep noise drops and is reviewed by bucket.
- Adding a stdlib member no longer edits `resolve.rs`.

## Immediate next checks

Use these as gates while refactoring:

```sh
grep -RIn "resolve_callable" src tests --exclude-dir=target
grep -RIn "descriptor()" src/types.rs src/resolve.rs src/ir_lower.rs src/module_symbols.rs
grep -RIn "java/lang\\|kotlin/jvm\\|\\$default\\|INSTANCE" src/types.rs src/resolve.rs src/ir.rs src/ir_lower.rs
grep -RIn "metadata_return_type\\|metadata_return_nullable\\|metadata_param_\\|metadata_kept_params" src
```

Current baseline on this tree after the thirty-second cleanup pass:

- `resolve_callable` hits in `src` + `tests`: 0
- `builtin_member_ret` / `builtin_member_call` hits in `src` + `tests`: 0
- `LibrarySet::member_return` / `LibrarySet::instance_call_return`: deleted
- `LibrarySet::sam_method`: deleted
- `LibrarySet::value_companion_fn`: deleted
- `LibrarySet::mangled_member`: deleted
- `LibrarySet::prim_companion_const`: deleted
- `LibrarySet::can_inline_lambda`: deleted
- `LibrarySet::coroutine_intrinsic`: deleted
- `LibrarySet::canonical_internal`: deleted
- `LibrarySet::builtin_member`: deleted
- `LibrarySet::can_inline_call`: deleted
- `LibrarySet` trait itself: deleted
- `member_return` / `instance_call_return` hits in `src` + `tests`: 2, both inside JVM member overload construction
- descriptor hits in non-backend type/check/lower modules: 221 broad hits / 30 narrow baseline
- JVM-name/default/`INSTANCE` hits in selected non-JVM type/check/IR/lower modules: 382 broad hits / 168 narrow baseline
- metadata probe hits in `src`: 24
- metadata return probes outside `src/jvm/classpath.rs` / `src/jvm/jvm_libraries.rs`: 0
- `resolve_scope_inline` hits in `src`: 0

Second-pass cleanup centralized the remaining checker receiver-extension compatibility path behind
`library_extension_callable` / `library_extension_return` and the extension-property getter side
channel behind `record_library_extension_property_getter`. That leaves `resolve.rs` with one old API
call inside the compatibility helper and one full top-level function-selection path that still needs
to be migrated to `FunctionSet`.

Third-pass cleanup mirrored that containment in the lowerer: receiver-extension lowering,
destructuring `componentN` extension fallback, operator extension fallback, and `iterator` extension
probes now route through `Lower::library_extension_callable` / `library_extension_return`. The
remaining old API calls in checker/lowerer are the two compatibility helpers and the two top-level
classpath-function selection paths; the other grep hits are comments.

Fourth-pass cleanup moved those top-level classpath-function selection paths behind
`CallResolver::resolve_top_level_callable`. At that point it still delegated to the old implementation
for behavior preservation, but checker/lowerer no longer reached through `LibrarySet::resolve_callable`
for receiver-less calls.

Fifth-pass cleanup reimplemented `CallResolver::resolve_top_level_callable` from `FunctionSet`
overload data and removed the top-level overload-selection body from `JvmLibraries::resolve_callable`.
The JVM library implementation now delegates receiver-less calls back to `CallResolver`; its remaining
old API body is the receiver-extension compatibility path. This removed the duplicate top-level
selection logic from the JVM layer and made `FunctionSet` the authoritative top-level callable query.
The generic-signature parser also moved next to `GSig` in `call_resolver`, so the JVM layer no longer
keeps a duplicate method-signature parser; deleting the old collection-return follow-up reduced
metadata probe hits by one.

Sixth-pass cleanup added `CallResolver::resolve_extension_callable`, with an exact-arity
`FunctionSet` selector for clear receiver-extension calls and a compatibility fallback for defaulted
or ambiguous extension cases. Checker/lowerer now route classpath extension calls through the resolver
instead of calling `LibrarySet::resolve_callable` directly; the only remaining hits in those files are
comments. The remaining old implementation body is isolated behind the resolver fallback and should
shrink next by representing extension `$default` calls in `FunctionInfo`.

The old JVM exact-extension selection branch was then deleted from `JvmLibraries::resolve_callable`.
That method is now a compatibility shim back into `CallResolver`; exact receiver-extension resolution
is owned by `CallResolver` and driven by `FunctionSet`.

Seventh-pass cleanup moved metadata return class information onto `FunctionInfo::ret_class` and
removed the temporary `LibrarySet::metadata_return_type` follow-up probe. The selected overload now
carries the metadata fact needed to distinguish erased JVM collection returns (`List<T>` vs
`MutableList<T>`) and unsigned/value-class logical returns. This keeps return metadata in the
overload-construction layer rather than letting checker/resolver consumers ask the library again by
owner/name after selection.

Eighth-pass cleanup normalized `$default` callables while constructing `FunctionInfo`: JVM mask/marker
parameters are removed from the logical parameter list, the base function's metadata is attached, and
`LibraryCallable::default_call` is set on the overload. `CallResolver` no longer reparses `$default`
descriptors to rediscover the real parameters for top-level or extension default-argument calls.

Ninth-pass cleanup removed the final JVM descriptor parser dependency from `CallResolver`: the
non-public inline-only top-level selector now uses the logical params and physical return already
stored on `FunctionInfo::callable`. `CallResolver` still imports JVM helpers for generic-signature
parsing and descriptor narrowing, but it no longer calls `parse_method_desc`.

Tenth-pass cleanup moved descriptor-narrowing overload tie-breaking into `FunctionInfo::overload_rank`.
The JVM provider computes that rank while constructing overloads; source/module providers use zero.
`CallResolver` now sorts by provider data and no longer imports `jvm_libraries::descriptor_narrowing`.
Its only remaining JVM import is the generic-signature class-name normalization used by the temporary
JVM signature parser.

Eleventh-pass cleanup moved parsed method generic signatures onto `FunctionInfo::generic_sig` and
moved JVM signature parsing back behind the JVM provider. `libraries.rs` now owns the platform-neutral
`GSig`/`GenericSig` data shape; `CallResolver` only performs backend-agnostic unification and
substitution over that parsed tree. Current `CallResolver` JVM imports: 0.

Twelfth-pass cleanup deleted the old JVM-only inline extension selector (`JvmLibraries::extension_callable`).
`resolve_scope_inline` now uses `CallResolver::resolve_extension_inline_callable`, so normal and
inline-only extension calls share the same `FunctionSet` selection and generic return binding. Two
JVM-specific behaviors moved into provider data construction instead of a second selector:
non-public `Object`-erased extensions are admitted only when their receiver is a type variable
(`T.let`/`takeIf`, not concrete value-class `Result.map`), and metadata return classes are applied to
extension results only for unsigned/value-class logical returns rather than by owner/name across an
overloaded stdlib family (`IntProgression.step` vs `CharProgression.step`). Added
`tests/resolver_regression_e2e.rs` to pin both issues plus lambda-return overload isolation
(`sumOf` must resolve through the lambda-return resolver; `forEach`/`map`/`fold` must remain ordinary
inline extension calls). Added dependency-free `KRUSTY_TRACE=resolve` tracing so future resolver
debugging does not require temporary `eprintln!` patches.

Thirteenth-pass cleanup moved metadata return class onto the selected `LibraryCallable`, not only the
wrapping `FunctionInfo`. The checker's no-lambda inline-extension guard now rejects unsupported
unsigned metadata returns (`UByte`/`UShort`/`UInt`/`ULong`) from the selected callable data instead of
calling back into `LibrarySet::metadata_return_unsigned(owner, name)`. That trait method and its JVM
implementation were deleted, and `resolver_regression_e2e` now pins the `toUShort` rejection so this
does not silently become a wrong splice. The JVM-only `Classpath::metadata_return_ty` projection was
also deleted; suspend logical return recovery now derives from the already-selected return class in
the provider. Metadata probe hits dropped from 25 to 24.

Fourteenth-pass cleanup deleted `LibrarySet::lambda_return_overload_param`. The checker now asks
`CallResolver::lambda_return_overload_param_types`, which derives the selector lambda's parameter
types from the special `FunctionSet` family used for `@OverloadResolutionByLambdaReturnType`
(`receiver_rank = u32::MAX`) and the already-parsed `FunctionInfo::generic_sig`. This keeps
`sumOf` pre-typing on the same provider data that later selects `sumOfInt`/`sumOfLong`, instead of
using a separate JVM facade/name probe.

Fifteenth-pass cleanup deleted `LibrarySet::toplevel_lambda_param_types`. Receiver-less top-level HOF
lambda pre-typing now goes through `CallResolver::top_level_lambda_param_types`, binding the selected
top-level `FunctionInfo::generic_sig` against already-typed non-lambda arguments. That keeps
`applyIt(5) { it + 1 }`-style typing in the same arg-dependent resolver layer as ordinary call
selection instead of re-parsing JVM signatures in `JvmLibraries`.

Sixteenth-pass cleanup deleted `LibrarySet::extension_lambda_param_types`. Extension HOF lambda
pre-typing now goes through `CallResolver::extension_lambda_param_types`, using `FunctionSet`
receiver ranks, visibility/`MustInline` flags, and `FunctionInfo::generic_sig` to bind receiver and
non-lambda arguments before typing lambda bodies. This preserves the old public-first behavior and
the scope-function fallback without letting `JvmLibraries` re-walk facade extension indexes and parse
signatures a second time.

Seventeenth-pass cleanup deleted `LibrarySet::toplevel_lambda_recvs`. `CallSig` now carries
`lambda_receivers`, populated once during `FunctionInfo` construction from metadata receiver-function
parameter facts. The checker asks `CallResolver::top_level_lambda_receivers`, so receiver-lambda
binding for classpath top-level HOFs travels with the rest of the selected call shape instead of
through a separate metadata probe. This finishes the lambda metadata probe migration; the remaining
lambda pre-typing helpers are resolver methods over `FunctionSet`.

Eighteenth-pass cleanup deleted `LibrarySet::resolve_scope_inline`. Checker and lowerer inline-only
extension paths now call `CallResolver::resolve_extension_inline_callable` directly, so public
extension calls and non-public `@InlineOnly` splice candidates share the same `FunctionSet`
selection logic without a JVM shim trait method. Backend splice capability remains behind
the platform-specific dry-run gates (`can_inline_lambda` / `can_inline_call` at this point). Verified with
`./run-tests.sh --test inline_splice_e2e --test classpath_receiver_lambda_e2e --test resolver_regression_e2e`.

Nineteenth-pass cleanup deleted `LibrarySet::resolve_callable`. The last test user now constructs a
`CallResolver` and calls `resolve_extension_callable` directly; the JVM wrapper implementation was
removed with the trait method. `resolve_callable` is now absent from `src` and `tests`, which makes
package-level callable selection exclusively a `CallResolver` concern over `FunctionSet`. Migrating
the metadata test also exposed a provider-ordering bug: metadata-public value-class inline extensions
(`Result.getOrThrow`) must be promoted before the generic non-public erased-`Object` guard runs, or
the consolidated inline resolver rejects a valid candidate. Verified with
`./run-tests.sh --test metadata_reader_e2e --test inline_splice_e2e --test classpath_receiver_lambda_e2e --test resolver_regression_e2e`
and `./run-tests.sh --test result_e2e`.

Twentieth-pass cleanup replaced the split builtin member APIs with one `BuiltinMember` handle.
`LibrarySet::builtin_member_ret` and `LibrarySet::builtin_member_call` are gone; checker paths read
the logical `ret`, while lowerer paths use the same selected handle's owner/JVM name/descriptor,
interface flag, and erased `physical_ret`. This keeps `.kotlin_builtins` member facts together and
removes the risk that checker and emitter select the same builtin member through different queries.
Verified with
`./run-tests.sh --test operator_index_e2e --test collection_members_e2e --test destructure_e2e --test feature_box_e2e`.

Twenty-first-pass cleanup introduced `CallResolver::resolve_instance_member`, a selected member handle
with logical `ret` and erased `physical_ret`. Checker/lowerer sites no longer resolve an instance
member and then make a second provider call to `member_return` or `instance_call_return`; those
generic-return refinements are centralized behind the resolver wrapper. The trait methods still exist
inside the provider boundary while member `FunctionInfo` gains the missing generic hierarchy data, but
the old side channel is no longer exposed to checker/lowerer code. Verified with
`./run-tests.sh --test shadowed_method_tparam_e2e --test generic_signature_e2e --test serialization_krusty_only_e2e --test destructure_e2e --test operator_index_e2e --test collection_members_e2e`.

Twenty-second-pass cleanup deleted `LibrarySet::member_return` and
`LibrarySet::instance_call_return`. `LibraryMember` now carries the backend generic signature, member
`FunctionInfo` stores parsed `generic_sig`, and `CallResolver::resolve_instance_member` selects the
member overload directly from `FunctionSet`. Receiver-bound returns (`List<String>.get(Int): String`)
stay on the selected callable; argument-bound erased returns (`decodeFromString(serializer, text): T`)
are refined by the resolver from the selected member signature. The only remaining `member_return`
helper is private to `JvmLibraries` while constructing member overload data from JVM class generic
hierarchies. Verified with
`./run-tests.sh --test shadowed_method_tparam_e2e --test generic_signature_e2e --test serialization_krusty_only_e2e --test destructure_e2e --test operator_index_e2e --test collection_members_e2e`.

Twenty-third-pass cleanup deleted `LibrarySet::sam_method`. The single abstract method of a functional
interface now lives on `LibraryType::sam_method`, so checker/lowerer SAM conversion and classpath
bridge generation read it from the same resolved type shape as publicness, kind, members, supertypes,
constructors, and companion facts. Added `tests/sam_classpath_e2e.rs` to pin Java `Runnable` SAM
lambda construction and the `Comparable<T>` bridge path. Verified with
`./run-tests.sh --test sam_classpath_e2e` and
`./run-tests.sh --test feature_box_e2e --test classpath_companion --test sam_classpath_e2e`.

Twenty-fourth-pass cleanup deleted `LibrarySet::value_companion_fn`. Public inline companion functions
on classpath value classes (`Result.success`) now live on `LibraryType::value_companion_fns` beside
the companion object field, SAM method, constructors, members, and value-class underlying type. The
checker selects the stored companion callable by source name and logical parameter count without
parsing JVM descriptors or making a second provider query. Verified with
`./run-tests.sh --test result_e2e --test metadata_reader_e2e --test suspend_e2e`.

Twenty-fifth-pass cleanup deleted `LibrarySet::mangled_member`. Unsigned range/progression lowering
now resolves mangled inline-class getter names by walking `LibraryType::members` and `supertypes`,
instead of asking a JVM-specific trait method to scan class files by prefix. This keeps real member
names/descriptors in the resolved type shape; the remaining lowerer helper is local selection over
that structured data. Verified with
`./run-tests.sh --test range_step_e2e --test unsigned_toplevel_e2e --test resolver_regression_e2e`.

Twenty-sixth-pass cleanup deleted `LibrarySet::prim_companion_const`. Primitive companion constants
(`Int.MAX_VALUE`, `Double.NaN`, …) now live on `LibraryType::companion_consts`, populated by the
provider while resolving the primitive type shape. Lowering still preserves source shadowing for bare
primitive names, but it reads the compile-time value from the resolved type data instead of asking a
platform-specific side channel to scan `kotlin/jvm/internal/*CompanionObject` fields. Verified with
`./run-tests.sh --test companion_const_e2e --test feature_box_e2e --test range_step_e2e`.

Twenty-seventh-pass cleanup deleted `LibrarySet::can_inline_lambda`. The JVM implementations of
`can_inline_lambda` and `can_inline_call` had converged to the same `splice_unified` dry-run over every
function-typed parameter. Lambda-taking inline extension routing now uses `can_inline_call`, leaving one
backend splice-capability hook instead of two equivalent trait methods. Verified with
`./run-tests.sh --test inline_splice_e2e --test classpath_receiver_lambda_e2e --test resolver_regression_e2e`.

Twenty-eighth-pass cleanup deleted `LibrarySet::coroutine_intrinsic`. Coroutine intrinsics are compiler
facts, not classpath-provider facts: the enum and recognition table now live together in `libraries.rs`,
while the checker/lowerer ask the shared table directly and the JVM backend-specific bytecode remains in
lowering. The obsolete `src/jvm/coroutine_intrinsics.rs` wrapper module was removed. Verified with
`./run-tests.sh --test coroutine_intrinsics_e2e --test suspend_e2e --test resolver_regression_e2e`.

Twenty-ninth-pass cleanup deleted `LibrarySet::canonical_internal`. Canonical type-name aliases now
travel with `LibrarySeed` as shared `canonical_names` data and are stored on `SymbolTable`, so subtype
checks fold Kotlin/JVM built-in aliases without calling back into a backend name map. The JVM provider
still owns construction of those aliases from its class map, but resolver consumers only see seeded
symbol data. Verified with
`./run-tests.sh --test collection_members_e2e --test generic_signature_e2e --test feature_box_e2e`.

Thirtieth-pass cleanup deleted `LibrarySet::builtin_member`. Kotlin built-in member handles now live on
`LibraryType::builtin_members`, populated from `.kotlin_builtins` while the provider resolves the type
shape. Checker and lowerer call sites select from resolved type data (`LibraryType::builtin_member`)
instead of making a second provider query by internal/name/args. `LibrarySet` now has one remaining
method: the backend inline-splice dry-run gate. Verified with
`./run-tests.sh --test collection_members_e2e --test operator_index_e2e --test feature_box_e2e --test destructure_e2e`.

Thirty-first-pass cleanup deleted `LibrarySet::can_inline_call`. Checker/lowerer consumers now route
selected inline calls from `InlineKind` only; there is no separate "can splice" model in resolver data.
The design invariant is that every selected inline function must be splicable. If the current JVM
splicer cannot handle a body, that is a backend/compiler gap to fix, not a semantic reason to hide the
call from resolution. The JVM backend now reports selected-inline splice failure as a specific inline
backend error instead of using the old silent unsupported-construct skip path. Verified with
`./run-tests.sh --test inline_splice_e2e --test classpath_receiver_lambda_e2e --test resolver_regression_e2e --test metadata_reader_e2e --test result_e2e`.

Thirty-second-pass cleanup deleted the marker-only `LibrarySet` trait. The compiler's provider
boundary is now `SymbolSource` directly: `SymbolTable` stores `Box<dyn SymbolSource>`,
`CallResolver` binds over `dyn SymbolSource`, and the empty provider is `EmptySymbolSource`.
`src` no longer contains `LibrarySet`; historical review entries only record deleted methods.

Thirty-third-pass inline cleanup fixed the mandatory-splice invariant instead of reintroducing a
`can splice` flag. The JVM inliner now treats synthesized function-reference and property-reference
objects as splice sources for inline higher-order calls: `list.map(c::inc)` and `list.map(C::n)` lower
to direct spliced bodies just like literal lambdas. The generic no-lambda splicer also keeps reference
parameters verifiable when an inline body's source pool lacks an exact frame class by falling back to
`java/lang/Object`, which unblocked vararg inline bodies such as `mapOf(Pair...)`. Added a focused
callable-reference regression in `tests/callable_ref_e2e.rs`. Verified with
`./run-tests.sh --test callable_ref_e2e`,
`./run-tests.sh --test feature_box_e2e`, and
`./run-tests.sh --test inline_splice_e2e --test classpath_receiver_lambda_e2e --test resolver_regression_e2e --test collection_members_e2e --test sam_classpath_e2e --test feature_box_e2e`.

Thirty-fourth-pass primitive-boundary cleanup removed the checker-local hardcoded primitive companion
constant table (`Int.MAX_VALUE`/`Double.NaN`/etc.). `LibraryType::companion_consts` now carries both the
constant value and its Kotlin type (`LibraryConst { ty, value }`), populated by the JVM provider from
the companion object's `ConstantValue` fields. The resolver reads the constant type from the resolved
type shape, and lowering reads the same fact to inline the value, including `Char` constants without a
name-based `Char` special case. The lookup still preserves source shadowing: a user `class Int` does
not fall through to `kotlin.Int.MAX_VALUE`. Verified with
`./run-tests.sh --test companion_const_e2e`,
`./run-tests.sh --test feature_box_e2e --test range_step_e2e --test metadata_reader_e2e`, `cargo check`,
and `cargo fmt --check`.

Primitive-boundary audit: Kotlin built-in scalar types (`Int`, `Boolean`, `Char`, ...) are acceptable
front-end facts, but JVM primitive representation is not. Remaining leaks to reduce include
`Ty::descriptor`, `Ty::is_primitive`/boxing helpers, `java/lang/*` names, slot-width assumptions, and
`kotlin/jvm/functions/*` checks in `resolve.rs`/`ir_lower.rs`. New work should move representation facts
behind provider/backend data structures rather than add more scalar-name or descriptor switches in the
checker/lowerer.

Thirty-fifth-pass generic-signature cleanup added a platform-neutral `GSig::Function { params, ret }`
node. The JVM signature parser now normalizes `kotlin/jvm/functions/FunctionN<..., R>` into that node,
so generic call resolution no longer recognizes function-typed metadata by matching a JVM class name.
`CallResolver` unifies and substitutes function signatures structurally, and the remaining erased
`FunctionN` arity check is isolated to one helper for ordinary callable parameter matching. Verified with
`./run-tests.sh --test generic_hof_method_check --test classpath_receiver_lambda_e2e --test callable_ref_e2e`,
`./run-tests.sh --test feature_box_e2e --test metadata_reader_e2e --test resolver_regression_e2e`,
`cargo check`, and `cargo fmt --check`.

Thirty-sixth-pass primitive-boundary cleanup moved boxed primitive normalization for generic function
signatures into the JVM provider. `parse_gsig` now converts boxed `FunctionN` type arguments such as
`java/lang/Integer`/`kotlin/Int` to `GSig::Prim(Ty::Int)` while constructing `GSig::Function`, so
`CallResolver::function_input_types` no longer knows JVM wrapper class names or performs a second
unboxing step. Wrapper names remain only in JVM-owned library parsing. Verified with
`./run-tests.sh --test generic_hof_method_check --test classpath_receiver_lambda_e2e --test callable_ref_e2e`,
`./run-tests.sh --test feature_box_e2e --test metadata_reader_e2e --test resolver_regression_e2e`,
`cargo check`, and `cargo fmt --check`.

Thirty-seventh-pass function-boundary cleanup removed erased `kotlin/jvm/functions/FunctionN`
recognition from `CallResolver`. The JVM provider now normalizes descriptor-level function interfaces
to semantic `Ty::Fun([Any; N], Any)` in `desc_to_ty`, and classpath metadata alignment maps
`kotlin/FunctionN` source parameter types to the same semantic shape. Lambda applicability is now an
arity check between function types, not a resolver-side JVM-name probe. The fix also kept metadata
overload alignment from truncating two-argument inline functions such as `require(cond) { message }`.
Verified with `./run-tests.sh --test feature_box_e2e`,
`./run-tests.sh --test generic_hof_method_check --test classpath_receiver_lambda_e2e --test callable_ref_e2e`,
`./run-tests.sh --test metadata_reader_e2e --test resolver_regression_e2e --test inline_splice_e2e --test collection_members_e2e`,
`cargo check`, and `cargo fmt --check`.

Thirty-eighth-pass checker-boundary cleanup removed the remaining resolver-side
`Lkotlin/jvm/functions/Function` descriptor scan from inline-extension selection. The checker now uses
the selected callable's semantic parameter list (`Ty::Fun`) to reject function-typed value parameters in
the no-lambda primitive inline fallback. The same pass removed local checker lists for
`String : CharSequence/Comparable/Serializable` and primitive-to-`Number`: those assignments now route
through the provider-backed subtype walker using `Ty::boxed_ref`/`obj_is_subtype`, leaving hierarchy
facts in library data instead of checker conditionals. Verified with
`./run-tests.sh --test feature_box_e2e --test collection_members_e2e --test resolver_regression_e2e --test metadata_reader_e2e --test inline_splice_e2e`,
`cargo check`, and `cargo fmt --check`.

Thirty-ninth-pass metadata-return cleanup removed the inline-only top-level return check that detected
`Nothing` by scanning for a `)Ljava/lang/Void;` descriptor suffix. `CallResolver` now realizes
`kotlin/Nothing` from `FunctionInfo::ret_class` through the same metadata-return helper used by other
classpath calls, so `error()`/`TODO()`-style callees get their logical bottom type from metadata rather
than from a JVM return descriptor. Verified with
`./run-tests.sh --test feature_box_e2e --test resolver_regression_e2e --test metadata_reader_e2e`,
`cargo check`, and `cargo fmt --check`.

Fortieth-pass metadata-return cleanup moved metadata return class decoding out of `CallResolver`.
`FunctionInfo::ret_class`/`LibraryCallable::ret_class` now carry a semantic `Ty` populated by the
provider, not a raw `kotlin/...` class-name string. The JVM provider owns the mapping from metadata class
names to `Ty` (including `Nothing`), and core selection only reattaches generic type arguments or checks
logical-vs-physical return shape. This removed the resolver's `kotlin/Int`/`kotlin/UInt`/primitive-name
table and the new no-lambda inline fallback no longer asks `is_unsigned()` to reject JVM-erased value
returns; value returns fail because their logical `ret` no longer equals the physical primitive return.
Verified with
`./run-tests.sh --test feature_box_e2e --test resolver_regression_e2e --test metadata_reader_e2e --test generic_hof_method_check --test classpath_receiver_lambda_e2e`,
`cargo check`, and `cargo fmt --check`.

Forty-first-pass boundary cleanup moved the remaining selected-call value-class representation query out
of `CallResolver`. `SymbolSource::value_underlying(Ty)` now owns the answer; the JVM provider supplies
JVM builtins such as unsigned primitives, while the default implementation handles ordinary `Ty::Obj`
value classes through `resolve_type`. The same pass added `SymbolSource::function_like_arity(Ty)` so
property references can keep their property-reference APIs (`get`, `name`) while still fitting
function-typed parameters without resolver/checker string probes for `KProperty0/1`. Verified with
`./run-tests.sh --test callable_ref_e2e --test feature_box_e2e --test classpath_receiver_lambda_e2e --test resolver_regression_e2e --test inline_splice_e2e`,
`cargo check`, and `cargo fmt --check`.

Forty-second-pass property-reference cleanup moved construction of the property-reference API type out
of `resolve.rs`. `SymbolSource::property_reference_type(arity, mutable)` now supplies the provider-owned
type for `::prop`, `obj::prop`, and `Type::prop`; the JVM provider maps that to `KProperty0/1` or
`KMutableProperty0/1`. Resolver no longer carries those JVM internal names, and the new
`property_ref_keeps_api_and_fits_function_shape` regression verifies that a property reference still
supports `.get`/`.name` while flowing into function-typed locals, user HOFs, and stdlib `map`. Verified
with
`./run-tests.sh --test callable_ref_e2e --test feature_box_e2e --test classpath_receiver_lambda_e2e --test resolver_regression_e2e --test inline_splice_e2e`,
`cargo check`, and `cargo fmt --check`.

Forty-third-pass class-literal cleanup moved expression-level class-literal result typing behind
`SymbolSource::class_literal_type()`. `resolve.rs` no longer names `java/lang/Class` for `User::class`;
the JVM provider supplies that target type. The remaining `KClass` type-ref path still carries a JVM
representation in `ty_of_ref` and is a follow-up candidate because that helper does not yet receive a
provider. Added `class_literal_type_is_provider_backed` to keep direct `C::class` coverage outside the
large feature corpus. Verified with
`./run-tests.sh --test callable_ref_e2e --test feature_box_e2e --test classpath_receiver_lambda_e2e --test resolver_regression_e2e --test inline_splice_e2e`,
`cargo check`, and `cargo fmt --check`.

Forty-fourth-pass type-ref/inline cleanup removed the remaining `KClass -> java/lang/Class` mapping from
`ty_of_ref`. Signature collection now creates a `TypeRefCtx` from provider data
(`SymbolSource::class_literal_type`) and passes it into `ty_of_ref_with`, so even early signature facts
use provider-owned target representation. The same pass fixed a systemic inline metadata leak:
selected classpath overloads now use `Classpath::is_inline_callable(owner, name, descriptor)`, not the
old descriptor-agnostic inline-name set, so a non-inline overload such as vararg `listOf(Object[])` no
longer inherits inline status from a same-name sibling and trips mandatory backend splicing. The
descriptor-less metadata fallback now requires a full source-parameter match, preserving inline-only
`joinToString(...){...}` while blocking zero-arg inline overloads from matching vararg siblings.
Verified with `cargo fmt --check`, `cargo check`, `./run-tests.sh --test feature_box_e2e`,
`./run-tests.sh --test inline_splice_e2e`, and
`./run-tests.sh --test callable_ref_e2e --test classpath_receiver_lambda_e2e --test resolver_regression_e2e --test serialization_krusty_only_e2e`.

Forty-fifth-pass inline/descriptor cleanup removed the stale descriptor-agnostic inline query entirely.
`Classpath` no longer caches or exposes `is_inline_method(owner, name)`; the only inline metadata path is
the selected-call `is_inline_callable(owner, name, descriptor, params)` check. This keeps overload-wide
inline decisions from returning later and removes one cache keyed only by class/name. The same pass moved
the remaining public JVM parameter-key formatter out of `resolve.rs`: lowering now asks
`jvm::names::params_descriptor`, and `ModuleSymbols` reuses `jvm::names::method_descriptor` instead of
open-coding another descriptor builder. Current focused gates:

```sh
grep -R -n "erased_params_key\|is_inline_method\|inline_names" src tests docs
grep -R -n "descriptor()" src/resolve.rs src/module_symbols.rs
```

Forty-sixth-pass `StringBuilder` cleanup removed the resolver-side `java/lang/StringBuilder` behavior
table and constructor shortcut. Source-level `kotlin.text.StringBuilder` is a Kotlin alias owned by the
provider/type-index path; on the JVM that alias resolves to the platform class, and member/constructor
selection then uses ordinary `LibraryType`/`FunctionSet` data. The AST-to-IR phase no longer treats
`StringBuilder` as a special language concept. Added `KotlinTextStringBuilderAlias` to the consolidated
feature harness so the Kotlin spelling exercises constructor calls, member calls, and receiver-lambda
member resolution without a resolver-side Java internal-name branch. Focused gate:

```sh
grep -R -n 'java/lang/StringBuilder\|resolve_stringbuilder_instance' src/resolve.rs
```

The desired trend is that each PR removes hits from these gates. A feature implementation that adds
new hits outside `src/jvm/**` should be treated as architecture regression unless it is classified as
front-end language syntax.

Forty-seventh-pass source-form/String cleanup removed another pair of special-case escape hatches instead
of adding a bail. The parser now records which `Expr::Call` ids came from infix syntax, so resolver and
lowering can model Kotlin's real distinction for primitive builtin names: `5 rem 2` may dispatch to an
exact user infix extension, while `5.rem(2)` keeps the primitive builtin member. This fixes the box
corpus `operatorConventions/infixFunctionOverBuiltinMember.kt` without a skip and prevents future
"dot-vs-infix" ambiguity branches from being solved by guessing. The same pass removed the checker-side
`java/lang/String` subtype probe; `String` assignability now asks the provider-backed semantic
`kotlin/String` hierarchy. Focused gates:

```sh
grep -n '"java/lang/String"' src/resolve.rs src/ir_lower.rs src/libraries.rs src/symbol_source.rs
./run-tests.sh --test resolver_regression_e2e -- --nocapture
./run-tests.sh --test string_concat_append_overload_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Current conformance signal from the harness after this pass:

```text
scanned: 7351 | krusty-compiled: 1840 | box()=OK: 1826 | skipped: 5511 | FAIL: 14
```

The conformance binary still exits nonzero because the 14 known failures remain, but coverage improved
without increasing failures. The next deletion candidate is descriptor construction in `ir_lower.rs`:
every `Ty::descriptor()` use outside `src/jvm/**` should either become a selected callable/backend handle
or move into a named JVM lowering pass.

Forty-eighth-pass descriptor cleanup concentrated the first layer of JVM descriptor construction. Direct
`Ty::descriptor()` calls are now absent from the common checker/lowerer/provider surface:

```sh
grep -R -n "\.descriptor()" src/ir_lower.rs src/resolve.rs src/module_symbols.rs src/libraries.rs src/symbol_source.rs
```

The remaining descriptor use in common lowering goes through `jvm::names::{type_descriptor,
params_descriptor, method_descriptor}`. This is still a JVM dependency in `ir_lower`, so it is not the
final architecture. It is an intentional intermediate state: all descriptor formatting now has one gate
that can be moved into a JVM IR-lowering/backend-handle pass instead of being open-coded across source
lowering. Focused verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture
./run-tests.sh --test delegated_prop_e2e -- --nocapture
./run-tests.sh --test delegated_member_prop_e2e -- --nocapture
./run-tests.sh --test string_concat_append_overload_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Next deletion target: replace descriptor-carrying `Callee::{Static,Virtual,Special}` construction in
`ir_lower` with semantic call handles or a post-common JVM lowering pass. The gate should become:

```sh
grep -R -n "jvm::names\|type_descriptor\|method_descriptor\|params_descriptor" src/ir_lower.rs src/resolve.rs src/module_symbols.rs
```

Forty-ninth-pass same-file overload-key cleanup removed JVM descriptor keys from `ir_lower`'s own
function-id table. `Lower::fun_ids` is now keyed by `(name, Vec<Ty>)`, so same-file overload routing and
vararg array-function lookup no longer collapse source-level parameter types into JVM descriptor strings.
This is intentionally smaller than the full `Callee` split, but it removes descriptor formatting from
ordinary local function lookup and leaves descriptors only where lowering still emits backend call
tokens or bridge-erasure checks. Focused verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test overloaded_inferred_return_e2e -- --nocapture
./run-tests.sh --test inline_e2e -- --nocapture
./run-tests.sh --test resolver_regression_e2e -- --nocapture
./run-tests.sh --test generic_fn_e2e -- --nocapture
```

Next local gate: `params_descriptor` should disappear from `ir_lower` after bridge-erasure decisions move
into a JVM lowering pass or a backend-owned bridge planner.

Fiftieth-pass bridge-comparison cleanup removed `params_descriptor` from common lowering. The two erased
bridge checks now compare each parameter/return through the single `type_descriptor` helper instead of
building parameter-list descriptor keys. This does not solve the deeper issue that bridge planning is
still in `ir_lower`, but it shrinks the JVM surface and makes the remaining calls exactly the places that
need backend tokens. Focused verification:

```sh
grep -R -n "params_descriptor" src/ir_lower.rs src/resolve.rs src/module_symbols.rs
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test generic_signature_e2e -- --nocapture
./run-tests.sh --test inheritance_e2e -- --nocapture
./run-tests.sh --test generic_base_member_type_e2e -- --nocapture
./run-tests.sh --test companion_supertype_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Next gate: eliminate `method_descriptor` from `module_symbols.rs` by replacing descriptor-bearing module
callables with semantic module call handles, or by moving `ModuleSymbols`' backend-token materialization
into a JVM-owned adapter.

Fifty-first-pass ignored-data cleanup deleted `TypeInfo::ext_calls`. The checker wrote this
descriptor-bearing side table for module and library extension calls, but no lowering code read it; extension
calls are resolved again through the current `FunctionSet`/call-resolver path at lowering time. Keeping the
map made it look as if selected call data flowed from resolver to lowerer, while in practice it was dead
state and another place that forced module symbols to synthesize JVM descriptors. Removed the field, checker
storage, and all writes. Verification:

```sh
grep -R -n "ext_calls" src tests
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test extension_fun_e2e -- --nocapture
./run-tests.sh --test dotted_extension_receiver_e2e -- --nocapture
./run-tests.sh --test resolver_regression_e2e -- --nocapture
./run-tests.sh --test string_concat_append_overload_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

This reinforces the larger direction: replace side tables with one `ExprResolution`/`ResolvedCall` table
that is actually consumed by lowering. The next similar audit target is `expr_lowers`: it should keep
absorbing selected expression facts until it can become a real unified expression-resolution table.

Current side-table audit status:

- `ext_calls`: deleted; it was written by the checker and never read.
- `expr_lowers`: still read by lowering for selected expression forms that cannot be recovered from
  shape alone. It now combines the old `obj_value_refs`, `ext_prop_calls`, `local_call_map`, and
  `inline_calls` maps, plus the old `lambda_info`, with explicit `LocalFunction`, `InlineCall`,
  `Lambda`, `ObjectValue`, and `ExtensionPropertyGet` variants; it should eventually become part of a
  unified expression-resolution table.
- `stmt_lowers`: still read by statement lowering for selected statement forms. It currently carries
  `PlusAssign`, replacing the old `plus_assign` set.

Fifty-second-pass extension-property cleanup split selection from recording for classpath extension
properties. `library_extension_property_getter` is now a pure selector returning the provider-selected
callable; only `check_member` writes `ext_prop_calls` when it has a concrete member expression id to lower.
At that point this did not remove the side table, but it removed the misleading `Option<ExprId>`
side-effect API and made the later `ExprLowering::ExtensionPropertyGet` replacement clearer.
Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test extension_property_e2e -- --nocapture
./run-tests.sh --test var_extension_property_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Fifty-third-pass extension-property payload cleanup narrowed `ext_prop_calls` from an anonymous
`(owner, method, descriptor)` tuple to the selected `LibraryCallable`. Lowering still emits the same static
getter, but the table now carries the same callable abstraction used elsewhere instead of a second ad hoc
shape. This is one step toward replacing the map with a real `ResolvedProperty` entry. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test extension_property_e2e -- --nocapture
./run-tests.sh --test var_extension_property_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Fifty-fourth-pass companion-call payload cleanup removed the duplicate method fields from
`CompanionFn`. Value-class companion calls (`Result.success`) now keep only the companion receiver
metadata (`class_internal`, `companion_internal`, `companion_field`) plus the selected `LibraryCallable`
for owner/name/descriptor/params/return/inline policy. The checker selects against `callable.name` and
`callable.params`; lowering emits through `callable.owner`, `callable.name`, `callable.descriptor`, and
`callable.inline`. This keeps the side table as one selected call payload instead of a second
companion-specific callable shape. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test result_e2e -- --nocapture
./run-tests.sh --test value_class_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Fifty-fifth-pass inline-call side-table consolidation merged `companion_calls` and `receiver_lambdas`
into one `TypeInfo::inline_calls: HashMap<ExprId, InlineCall>`. The checker now records
`InlineCall::ValueCompanion` for value-class companion methods and `InlineCall::ReceiverLambda` for
`run`/`apply`/`with`; lowering has one pre-normal-call arm that consumes the selected variant. This
removes one expression-keyed side table and one name-shaped special-case path while preserving the
semantics that cannot legally fall through to ordinary call lowering. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test result_e2e -- --nocapture
./run-tests.sh --test value_class_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Fifty-sixth-pass value-class metadata member cleanup fixed unsigned conversion resolution without adding
another resolver hardcode for each `UInt`/`ULong` member. The JVM provider now exposes metadata-public
classpath value-class members (dropping the erased receiver from the logical parameter list), and metadata
type decoding preserves `kotlin/UInt`/`kotlin/ULong` as semantic `Ty::UInt`/`Ty::ULong` instead of
object types. A narrow checker conversion helper mirrors the existing lowering support for integral and
unsigned `toX` conversions, so `42.toUInt()` and `u.toInt()` typecheck before lowering emits the existing
conversion path. While running the full feature harness, `RangeValue` exposed a separate property-read
emission bug in the Kotlin/Java collection interop puzzle around `first` vs `first()`: builtins
properties now preserve that they came from property metadata and are exposed to the JVM side under their
accessor names (`first` property → `getFirst`, mapped collection properties such as `keys` → `keySet`,
plain Java-style properties such as `size` stay `size`). Property reads prefer accessor/mapped names, so
`range.first` emits `getFirst()`, while call syntax (`range.first()`, `list.first()`) no longer sees a
fake `first()` member and resolves through the ordinary function/extension path. Added
`unsigned_integral_conversions_resolve_from_metadata` and
`property_first_and_extension_first_call_do_not_collide` to `resolver_regression_e2e`. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test collection_members_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Fifty-seventh-pass lambda side-table consolidation merged `recv_lambda_tys` and the checker-side
`inline_lambdas` set into `TypeInfo::lambda_info: HashMap<ExprId, LambdaInfo>`. A lambda literal now has
one resolution payload carrying its receiver-function closure receiver (`receiver: Option<Ty>`) and its
capture mode (`Closure` vs `InlineSplice`). Lowering now performs one lookup to decide both the synthetic
receiver parameter and whether capture scanning should include nested lambdas. This removes one
expression-keyed table and prevents a receiver lambda checked in an inline context from splitting its
facts across two independent side channels. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test inline_e2e -- --nocapture
./run-tests.sh --test classpath_receiver_lambda_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Fifty-eighth-pass selected-expression lowering consolidation merged `obj_value_refs` and `ext_prop_calls`
into `TypeInfo::expr_lowers: HashMap<ExprId, ExprLowering>`. The checker now records
`ExprLowering::ObjectValue` for classpath `object` value reads and `ExprLowering::ExtensionPropertyGet`
for classpath extension-property getter calls; lowering consumes a single selected-expression payload for
both `Name` and `Member` expressions. This removes another expression-keyed map and keeps extension
properties as selected `LibraryCallable`s without a property-specific side channel. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test extension_property_e2e -- --nocapture
./run-tests.sh --test var_extension_property_e2e -- --nocapture
./run-tests.sh --test classpath_object_value_e2e -- --nocapture
./run-tests.sh --test serialization_krusty_only_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Fifty-ninth-pass local-function selection consolidation removed `TypeInfo::local_call_map`. Local
function calls and local function references now record `ExprLowering::LocalFunction { stmt_id }` in the
same selected-expression table as classpath object values and extension-property getters. The lambda
checker’s "closure calls a local function" guard now counts only `LocalFunction` entries in
`expr_lowers`, so unrelated selected expression facts inside the lambda do not trip it. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test callable_ref_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Sixty-second-pass inferred-return cleanup removed `TypeInfo::fun_ret_overrides`. Deeper checker
inference for unannotated expression-body top-level functions now stages `(name, params) -> ret` patches
inside the checker, uses those staged returns for same-file call typing, and writes the inferred return
back into the canonical `SymbolTable` signatures after the file check drops its immutable symbol borrow.
Lowering now emits from `Signature::ret` directly; inferred and annotated returns share the same data
path, and codegen no longer has a second return override lookup keyed by function name and parameters.
Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test generic_inferred_return_e2e -- --nocapture
./run-tests.sh --test overloaded_inferred_return_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Sixty-third-pass dead bridge side-table cleanup removed `TypeInfo::bridges` and the private
`BridgeSpec` payload. The checker no longer records bridge specs into a table that lowering does not
read; it only rejects unsupported bridge shapes. All live bridge emission stays in IR lowering, where the
lowerer has concrete function ids and already synthesizes method, property, and interface bridges from
the canonical symbol/IR data. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Sixty-fourth-pass local-capture cell cleanup moved local-function capture cell mode into the structured
`LocalCapture` payload as `shared_cell`. Local-function declaration lowering, local-function body
binding, and local-function reference/call lowering now consume that per-capture semantic bit instead of
joining `LocalFunInfo::captures` with the file-wide `shared_cell_vars` name set. This narrows the remaining
mutable-capture side channel to lambda/local-declaration lowering and keeps JVM holder terminology out of
the local-function checker payload. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test callable_ref_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Sixty-fifth-pass shared-cell boundary cleanup removed `shared_cell_vars` from `TypeInfo`. The checker no
longer exports a file-wide mutable-capture name set. Local functions keep their structured per-capture
`shared_cell` bit, while ordinary lambda/local declaration lowering computes shared-cell needs in
`ir_lower` with a scoped prepass over the function body: only a non-inline closure capturing a `var` from
an outer local scope marks that name for a shared cell. JVM `Ref` holder allocation remains a lowering
implementation detail behind the runtime hook. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test lambda_e2e -- --nocapture
./run-tests.sh --test callable_ref_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Sixty-sixth-pass selected-expression inline-call cleanup removed `TypeInfo::inline_calls`. Value-class
companion calls and receiver-lambda scope calls now record `ExprLowering::InlineCall(InlineCall)` in the
same selected-expression table as local-function calls, classpath object values, and extension-property
gets. Lowering has one expression selection lookup for these special expression forms instead of a
second `ExprId` map checked before ordinary call lowering. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test result_e2e -- --nocapture
./run-tests.sh --test value_class_e2e -- --nocapture
./run-tests.sh --test classpath_receiver_lambda_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Sixty-seventh-pass lambda-expression payload cleanup removed `TypeInfo::lambda_info`. Receiver-lambda
closure receivers and inline-splice capture policy now record `ExprLowering::Lambda(LambdaInfo)` in the
same selected-expression table as other expression lowering facts. The lowerer defaults missing lambda
payloads to ordinary closure behavior, but no longer carries a second expression-keyed lambda map.
Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test lambda_e2e -- --nocapture
./run-tests.sh --test inline_e2e -- --nocapture
./run-tests.sh --test classpath_receiver_lambda_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Sixty-eighth-pass outer-local capture cleanup tightened shared-cell discovery to the actual semantic
boundary: a mutable local needs a shared cell only when a non-inline lambda or lifted local function
accesses the local declared in an outer lexical scope. The scans now respect shadowing by lambda
parameters, catch parameters, loop variables, destructuring entries, and inner local declarations, so a
lambda-local `var x` no longer boxes an unrelated outer `var x`. A regression test also checks the
generated classes do not contain `kotlin/jvm/internal/Ref$IntRef` for the shadowed case. While exercising
local-function references, the JVM emitter was also corrected to hard-fail failed splices only for
`InlineKind::MustInline`; public `CanInline` stdlib calls with callable-reference arguments can fall back
to their public method body. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test feature_box_e2e -- --nocapture lambda_shadowed_outer_var_does_not_allocate_ref_cell
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test lambda_e2e -- --nocapture
./run-tests.sh --test inline_e2e -- --nocapture
./run-tests.sh --test local_fun_ref_e2e -- --nocapture
./run-tests.sh --test name_based_destructuring_e2e -- --nocapture var_component_captured_and_mutated_in_lambda
```

Sixty-ninth-pass statement-lowering payload cleanup removed the exported `TypeInfo::local_funs` map.
Lifted local-function declaration data is now `StmtLowering::LocalFunction(LocalFunInfo)` inside the
existing statement-lowering table, with `TypeInfo::local_fun(stmt_id)` as the only accessor. This leaves
`TypeInfo` with one expression type vector plus one expression-selection table and one
statement-selection table; local-function calls still point from `ExprLowering::LocalFunction` to the
owning statement id. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test local_fun_ref_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
```

Seventieth-pass default-import boundary correction keeps default imports out of target-runtime lowering.
The resolver owns Kotlin's common defaults in dotted package form, while the platform symbol source
contributes documented target additions (`JvmLibraries` adds `java.lang` and `kotlin.jvm`). The resolver
composes those lists and converts to internal package syntax for lookup, so import policy is not encoded
as backend runtime data. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test classpath_receiver_lambda_e2e -- --nocapture
```

Seventy-first-pass mapped-member source cleanup removed the resolver-owned collection accessor table
(`keys` -> `keySet`, `entries` -> `entrySet`). The JVM symbol source now matches those semantic Kotlin
property queries and returns the physical callable metadata, so resolver and lowering ask for the Kotlin
member name and consume the provider's owner/name/descriptor. The only remaining fallback is generic
JavaBean `getX` spelling for library metadata that lacks Kotlin property names. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test collection_members_e2e -- --nocapture
./run-tests.sh --test resolver_regression_e2e -- --nocapture property_first_and_extension_first_call_do_not_collide
./run-tests.sh --test classpath_receiver_lambda_e2e -- --nocapture
```

Seventy-second-pass property-member resolver cleanup centralized classpath/library property reads in
`call_resolver::resolve_property_member`. Resolver and lowering no longer compute physical getter names
for library properties; they ask for the Kotlin property name, and the symbol source supplies any
platform fallback spelling through `SymbolSource::physical_property_getter_name`. JVM JavaBean naming now
lives in `JvmLibraries`, beside mapped-member aliases and descriptor metadata. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test extension_property_e2e -- --nocapture
./run-tests.sh --test var_extension_property_e2e -- --nocapture
./run-tests.sh --test collection_members_e2e -- --nocapture
./run-tests.sh --test resolver_regression_e2e -- --nocapture
./run-tests.sh --test classpath_receiver_lambda_e2e -- --nocapture
```

Seventy-third-pass JVM property-accessor naming cleanup moved Kotlin/JVM JavaBean accessor spelling
into `jvm::names::{property_getter_name, property_setter_name}`. `ir_lower`, the JVM IR emitter,
value-class lowering, JVM library property fallback, and the serialization plugin now share one
backend-owned rule instead of carrying local `getX`/`setX` helpers. This also fixes value-class
boolean properties: `@JvmInline value class Flag(val isOpen: Boolean)` emits `isOpen()Z`, not
`getIsOpen()Z`. The regression lives in `tests/value_class_e2e.rs`. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test extension_property_e2e -- --nocapture
./run-tests.sh --test var_extension_property_e2e -- --nocapture
./run-tests.sh --test value_class_e2e -- --nocapture
./run-tests.sh --test plugins_e2e -- --nocapture
./run-tests.sh --test serialization_roundtrip_e2e -- --nocapture
./run-tests.sh --test serialization_krusty_only_e2e -- --nocapture
```

Seventy-fourth-pass counted-loop resolver cleanup removed the resolver's duplicate hardcoded table of
`IntRange`/`LongRange`/`CharRange`/unsigned range and progression element types. `for` loop variable
typing now asks `TargetRuntime::counted_loop_info`, the same provider-owned metadata that lowering uses
to emit counted loops and JVM accessor descriptors. This keeps range/progression platform/library shape
in the platform provider instead of teaching resolver concrete class names twice. Existing range
regressions cover the behavior; this pass is an architecture cleanup, not a new surface feature.
Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test range_step_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
```

Seventy-fifth-pass primitive-predicate removal deleted `Ty::is_primitive()` entirely. Front-end code no
longer asks a global "primitive" question that conflates Kotlin scalar types, unsigned value classes,
boxing, and JVM unboxed representation. Resolver/checker sites now spell their intent directly
(`boxed_ref`, signed numeric/Char matches, provider-known value-class returns), while JVM lowering and
emission use local `is_jvm_scalar` helpers where bytecode representation is actually being selected.
The `toUInt`/`toULong` path now resolves from stdlib extension metadata and preserves value-class
metadata returns instead of using a resolver conversion table; `Number.toInt` remains a normal builtin
member inherited through provider metadata. Verification:

```sh
grep -R -n "is_primitive" src
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_integral_conversions_resolve_from_metadata
./run-tests.sh --test bounded_type_param_e2e -- --nocapture number_bound_member_toint_resolves_and_runs
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
```

Seventy-sixth-pass common-lowering representation cleanup removed the `JvmScalarTy` extension trait
from `ir_lower`. Common lowering no longer makes JVM scalar representation look like a method on
`Ty`; it uses a private `has_jvm_scalar_repr` helper only at bytecode-shape boundaries that still need
IR coercions, zero placeholders, comparisons, or unsigned representation rewrites. This keeps the
previous `Ty::is_primitive` deletion from reappearing under a different method name in the common
lowerer while leaving the remaining platform-specific work visible for future extraction into
backend-owned lowering. Verification:

```sh
grep -n "trait JvmScalarTy\|impl JvmScalarTy\|\.is_jvm_scalar()" src/ir_lower.rs
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_integral_conversions_resolve_from_metadata
./run-tests.sh --test value_class_e2e -- --nocapture
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
```

Seventy-seventh-pass metadata-return binding cleanup consolidated call-resolver metadata return
overlay through `logical_ret_from_metadata`. Ordinary top-level calls, extension default calls,
top-level `@InlineOnly` calls, and now top-level `$default` calls all apply `ret_class` in the same
place instead of open-coding slightly different post-selection return recovery. This closes a missed
case where a defaulted top-level library function with an erased physical return (`Int`) and metadata
return (`UInt`/another value class or collection class) would type as the descriptor return. A new
unit regression proves the default-call path directly with a fake `SymbolSource`. Verification:

```sh
cargo test --profile gate call_resolver::tests::top_level_default_callable_preserves_metadata_return_type -- --nocapture
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_integral_conversions_resolve_from_metadata
./run-tests.sh --test classpath_default_args_e2e -- --nocapture
./run-tests.sh --test metadata_reader_e2e -- --nocapture
```

Seventy-eighth-pass property-reference signature cleanup moved the JVM reflection signature string for
synthesized property references behind `TargetRuntime::property_reference_signature`. Common lowering
now carries the logical property `Ty` for local delegates and asks the platform for the
`getter()descriptor` token when constructing `PropertyReference*Impl`; it no longer stores a
descriptor string in `LocalDelegate` or formats delegated-property `KProperty` signatures itself. The
JVM implementation keeps the existing spelling in one backend-owned place. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test delegated_local_prop_e2e -- --nocapture
./run-tests.sh --test delegated_prop_e2e -- --nocapture
./run-tests.sh --test delegated_member_prop_e2e -- --nocapture
./run-tests.sh --test toplevel_property_ref_e2e -- --nocapture
```

Seventy-ninth-pass local-delegate callable-state cleanup removed `LocalDelegate`'s cached
`getValue`/`setValue` descriptor strings. The lowering state now keeps the resolved `Signature`s and
formats the backend call descriptor only at the `Callee::Virtual` construction point, which also removes
the duplicate `setValue` lookup on every local delegated `var` assignment. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test delegated_local_prop_e2e -- --nocapture
```

Eightieth-pass top-level delegate lookup cleanup removed a duplicate `getValue` resolution in
`lower_delegated_top_level`. Pass 2 already has the resolved delegate `getValue` signature for the
actual call, so inferred delegated-property type recovery now uses that return directly instead of
calling back through `delegated_prop_type` and repeating the same member lookup. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test delegated_prop_e2e -- --nocapture
```

Eighty-first-pass descriptor-parse boundary cleanup removed the remaining direct
`jvm_libraries::parse_method_desc` calls from common lowering. Classpath instance-member lowering now
uses the selected `LibraryMember.params` already supplied by the provider for argument adaptation and
continues to pass the opaque descriptor token only to the backend call site. This keeps JVM descriptor
parsing inside the JVM library provider instead of reparsing backend strings in `ir_lower`. Verification:

```sh
grep -R -n "parse_method_desc" src/ir_lower.rs src/resolve.rs src/call_resolver.rs src/libraries.rs src/symbol_source.rs
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test java_instance_e2e -- --nocapture
./run-tests.sh --test collection_members_e2e -- --nocapture
./run-tests.sh --test generic_base_member_type_e2e -- --nocapture
```

Eighty-second-pass descriptor-owner staging moved the JVM descriptor algorithm into
`jvm::names::type_descriptor` instead of making that helper delegate back to `Ty::descriptor`. This is a
staging cleanup for removing descriptor knowledge from `Ty`: new JVM/backend code now has a backend-owned
implementation to call, while the old `Ty::descriptor()` compatibility wrapper can be retired in a
follow-up without changing descriptor behavior. Verification:

```sh
cargo fmt --check
cargo check --profile gate
cargo test --profile gate jvm::names -- --nocapture
```

Eighty-third-pass descriptor-callsite migration moved the straightforward JVM provider/backend
call-sites off `Ty::descriptor()` and onto `jvm::names::type_descriptor`. `jvm/backend`,
`jvm/classpath`, `jvm/jvm_libraries`, and `jvm/value_classes` now use the backend-owned descriptor helper;
the only remaining compatibility consumers are `types.rs` itself and the large JVM emitter, which can
be migrated separately. Verification:

```sh
grep -R -n "\.descriptor()" src/jvm/backend.rs src/jvm/classpath.rs src/jvm/jvm_libraries.rs src/jvm/value_classes.rs src/types.rs
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test metadata_reader_e2e -- --nocapture
./run-tests.sh --test value_class_e2e -- --nocapture
./run-tests.sh --test coroutine_intrinsics_e2e -- --nocapture
```

Eighty-fourth-pass JVM emitter descriptor migration started moving `jvm/ir_emit` off the compatibility
`Ty::descriptor()` method. The first batch covers early class/static/property/value-class emission sites
and routes them through `jvm::names::type_descriptor`, leaving the later expression-emitter and verifier
helpers for follow-up batches. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test value_class_e2e -- --nocapture
./run-tests.sh --test property_accessor_e2e -- --nocapture
./run-tests.sh --test class_body_e2e -- --nocapture
```

Eighty-fifth-pass JVM emitter descriptor migration continued the `jvm/ir_emit` move from
`Ty::descriptor()` to `jvm::names::type_descriptor`, covering annotation hash/toString helpers, enum
field/constructor signatures, and JVM generic method/bound signatures. The remaining emitter
compatibility calls are now concentrated in expression-emission and verifier helpers. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test enum_class_signature_e2e -- --nocapture
./run-tests.sh --test enum_entries_e2e -- --nocapture
./run-tests.sh --test generic_signature_e2e -- --nocapture
```

Sixtieth-pass selected-statement lowering cleanup replaced `TypeInfo::plus_assign` with
`TypeInfo::stmt_lowers: HashMap<StmtId, StmtLowering>`. Compound assignments selected as in-place
`opAssign` calls now record `StmtLowering::PlusAssign` instead of a standalone statement-id set. This is
the statement-level analogue of `expr_lowers`: the checker records the selected lowering decision in one
typed payload, and lowering consumes that payload before ordinary assignment lowering. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Sixty-first-pass local-function payload cleanup merged `local_fun_sigs` and `local_fun_captures` into
`TypeInfo::local_funs: HashMap<StmtId, LocalFunInfo>`. A lifted local function now has one declaration
payload carrying its mangled name, signature, and ordered captures, so declaration lowering, local
function references, and local function calls all consume the same fact instead of joining two maps by
`StmtId`. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run
./run-tests.sh --test callable_ref_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
```

Eighty-sixth-pass JVM emitter descriptor migration moved another expression-emission batch from
`Ty::descriptor()` to `jvm::names::type_descriptor`: synthetic function-reference invoke descriptors,
property-reference getter descriptors, instance/top-level field access, cross-file accessor calls, and
the inline receiver splice descriptor. This leaves the remaining `ir_emit` compatibility calls isolated
to capture-class descriptors, class-literal array handling, and verifier helpers. Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test callable_ref_e2e -- --nocapture
./run-tests.sh --test property_accessor_e2e -- --nocapture
./run-tests.sh --test class_body_e2e -- --nocapture
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_integral_conversions_resolve_from_metadata
```

Eighty-seventh-pass JVM descriptor boundary cleanup removed `Ty::descriptor()` from the core type model.
All production descriptor construction now goes through `jvm::names::type_descriptor`, and the descriptor
unit coverage moved from `types.rs` to `jvm/names.rs`. This completes the planned removal of the
core-facing JVM descriptor compatibility wrapper; `grep -R -n "\.descriptor()\|fn descriptor\|pub fn
descriptor" src tests` now finds only unrelated descriptor-parsing/helper names, not a `Ty` method or
callsite. Verification:

```sh
grep -R -n "\.descriptor()\|fn descriptor\|pub fn descriptor" src tests || true
grep -R -n "is_primitive" src tests || true
cargo fmt --check
cargo check --profile gate
cargo test --profile gate jvm::names -- --nocapture
./run-tests.sh --test callable_ref_e2e -- --nocapture
./run-tests.sh --test nullable_primitive_box_e2e -- --nocapture
```

Eighty-eighth-pass conformance safety cleanup restored the codegen/box gate to 0 FAIL while continuing
the JVM-boundary cleanup. `Ty::unsigned_repr()` was removed from the core type model; the only remaining
unsigned carrier mapping is a local JVM-lowering helper used by unsigned conversion/bitwise lowering.
The function-reference inline body path now resolves the empty facade sentinel before emitting class and
method refs, fixing top-level callable references passed through inline calls. Unsigned bitwise/shift
method calls now lower through the same primitive intrinsic path as signed `Int`/`Long`, using the local
carrier mapping instead of falling through to bogus virtual calls.

The conformance gate also now declines corpus cases whose semantics are not modeled rather than counting
invalid bytecode as compiled support: builder-inference directives, JS-runtime-only tests,
generic/advanced `Result<T>` shapes, unsupported `UByte`/`UShort` value classes, and
`WORKS_WHEN_VALUE_CLASS` advanced value-class cases. Simple `Result.success(...).getOrThrow()` remains
supported and covered by `result_e2e`; do not reintroduce a broad `Result` skip. The JVM emit gate
mirrors the production safety side for unsupported stdlib value classes, so these are skipped by
compilation rather than only by the harness. Current conformance metric from that pass:

```text
scanned: 7351 | krusty-compiled: 1556 | box()=OK: 1556 | skipped(unsupported): 5795 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_metadata_return_blocks_unsupported_inline_splice unsigned_integral_conversions_resolve_from_metadata
./run-tests.sh --test value_class_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-eighteenth-pass value-array checker cleanup removed one more resolver-side unsigned special
case. `Array(n) { ... }` no longer decides that value-class/scalar elements need logical `Array<T>` by
checking `elem.is_unsigned()` in the checker; it asks the federated library source for
`value_underlying(elem)`. That keeps the shape provider-owned and generalizes the path from builtin
unsigned carriers to any library/user value class whose underlying is known. A focused regression pins
`Array(3) { Vc(it + 1) }` so this does not collapse back to an unsigned-only branch.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test value_class_e2e -- --nocapture sized_array_of_value_class_uses_provider_value_underlying
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twenty-second-pass scalar/unsigned boxing cleanup moved another batch of JVM-shaped lowering
facts behind `TargetRuntime`. The runtime now exposes `unsigned_integer_box_type(ty)`, so common lowering
no longer spells the `kotlin/UInt`/`kotlin/ULong` owner names in the unsigned box/unbox helper, the
reference-to-unsigned guard, or `is UInt` type operands. The same pass migrated nullable-primitive
`!!`, safe-call/Elvis primitive fusion, smart-cast unboxing, mixed primitive promotion, and structural
`Any == scalar` boxing to `scalar_value_repr` / `is_unsigned_integer_type`. Remaining lowerer sites still
use the old helper, but the edited paths now route target representation through the runtime boundary.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test nullable_primitive_box_e2e -- --nocapture
./run-tests.sh --test unsigned_toplevel_e2e -- --nocapture
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_binary_operators_use_library_type_identity
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twenty-third-pass scalar query migration moved another low-risk lowerer cluster from the local
JVM scalar/unsigned helpers to provider-owned queries. Non-null casts, `as` primitive/reference
coercion, `when` subject/condition scalar-shape checks, unsigned string-template interpolation, generic
vararg primitive adaptation, default-call primitive argument grouping, and generic-constructor primitive
type-argument adaptation now use `scalar_value_repr` / `is_unsigned_integer_type`. This reduces direct
core references to unsigned/scalar type knowledge while leaving the deeper value-class lowering and
receiver-call paths for separate passes.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test nullable_primitive_box_e2e -- --nocapture
./run-tests.sh --test unsigned_toplevel_e2e -- --nocapture
./run-tests.sh --test generic_inferred_return_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture nullable_boxing_corpus_cases_box_ok unsigned_compare_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twenty-fourth-pass lowerer scalar table removal deleted the local `has_jvm_scalar_repr` /
`unsigned_jvm_repr` model from common lowering. The last call sites now use provider/runtime facts:
delegated-property erased boxing, external value-class underlying classification, extension receiver
boxing, collection element narrowing, named primitive operator dispatch, unsigned conversions,
collection element primitive adaptation, and generic-signature primitive-bound selection all query
`scalar_value_repr` / `is_unsigned_integer_type` / `unsigned_integer_box_type`. After this pass,
`grep -n "has_jvm_scalar_repr\\|\\.is_unsigned()\\|unsigned_jvm_repr" src/ir_lower.rs` is empty.

This does not mean every JVM nuance has left lowering; it means the duplicated local scalar/unsigned
table is gone. Remaining platform-shaped lowerer work should be found by searching concrete class names,
descriptors, and runtime helper names rather than the old scalar helper.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test nullable_primitive_box_e2e -- --nocapture
./run-tests.sh --test unsigned_toplevel_e2e -- --nocapture
./run-tests.sh --test generic_inferred_return_e2e -- --nocapture
./run-tests.sh --test generic_signature_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twenty-first-pass scalar-representation migration continued replacing lowerer-local primitive /
unsigned checks with provider-owned queries. The primitive operator-method path (`a.plus(b)`,
`a.compareTo(b)`, bitwise/shift members), generic iterator element unboxing, unsigned binary
`/`/`%`/ordered comparisons, unsigned string-concat conversion, and unsigned range constructor guards now
use `scalar_value_repr` / `is_unsigned_integer_type` instead of directly asking `Ty` for unsigned-ness or
consulting the local JVM scalar table. This removes another batch of core-lowering knowledge about
`UInt`/`ULong` carriers while keeping the emitted IR unchanged.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_binary_operators_use_library_type_identity primitive_builtin_infix_extension_source_form_matters
./run-tests.sh --test unsigned_toplevel_e2e -- --nocapture
./run-tests.sh --test value_class_e2e -- --nocapture sized_array_of_value_class_uses_provider_value_underlying
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Note: the broad table-driven `./run-tests.sh --test feature_box_e2e -- --nocapture feature_snippets_run`
was not used as a gate for this pass because unrelated pre-existing snippets still fail
(`TakeIfNullableResultKt`, `InlineWithHandlersKt`, and `AsToPrimitiveKt`). The newly added
`ValueClassSizedArrayKt` snippet did return `OK` in that combined run.

Hundred-nineteenth-pass unsigned operator typing cleanup removed the final direct `is_unsigned()` query
from the resolver. Binary operator checking still has the Kotlin unsigned arithmetic rule, but it now
asks the federated symbol source whether the operand is an unsigned integer library type instead of
matching `UInt`/`ULong` locally. The JVM stdlib provider owns that identity next to its existing
`value_underlying(UInt -> Int, ULong -> Long)` facts. This is still a stepping stone toward full
operator-as-call resolution, but it removes another core hardcode and keeps unsigned type identity out of
the checker.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_binary_operators_use_library_type_identity unsigned_integral_conversions_resolve_from_metadata
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twentieth-pass scalar-representation boundary cleanup started moving common lowering away from
the local `has_jvm_scalar_repr` table. `TargetRuntime` now exposes `scalar_value_repr(ty)`, implemented
by the JVM provider next to the mutable-ref/value-underlying runtime facts. Lowering uses that provider
query for argument boxing/coercion, default-placeholder constants, and erased-read unboxing, and uses the
symbol-source unsigned-type predicate for ordered unsigned comparisons. This is a partial migration: many
older lowerer paths still call the local helper, but the new API gives the next passes a target-owned
replacement rather than copying the primitive/unsigned table again.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_binary_operators_use_library_type_identity unsigned_integral_conversions_resolve_from_metadata
./run-tests.sh --test value_class_e2e -- --nocapture sized_array_of_value_class_uses_provider_value_underlying
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-eleventh-pass String/library resolution cleanup removed the front-end's curated
`resolve_string_instance` table. String members now resolve through the same library/member lookup as
other receivers, and String extensions use classpath metadata plus the existing inline-splice path
instead of a resolver-local method list. The important guardrail is that private `@InlineOnly`
extensions are not admitted by normal extension resolution globally: doing so lets unsupported private
stdlib helpers type-check and then be emitted as illegal calls. Only implicit-receiver extension lookup
uses the inline-only query, where lowering already knows it must splice the selected callable.

This fixed the table dependency for cases such as `fun String.f() = uppercase()` and
`"ab".run { uppercase() }` without changing the zero-fail conformance surface.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test string_concat_append_overload_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-sixteenth-pass callable metadata shape cleanup removed `LibraryCallable.ret_class`. Metadata
return type belongs to overload selection (`FunctionInfo.ret_class`), while the selected callable only
needs the logical return (`ret`) and the physical backend return (`physical_ret`) that emit/lowering
bridge between. This avoids carrying the same metadata return through both `FunctionInfo` and
`LibraryCallable`, and keeps `LibraryCallable` focused on the opaque emit handle plus selected
logical/physical signature.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test metadata_return_types -- --nocapture
./run-tests.sh --test string_concat_append_overload_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-seventeenth-pass extension-property selection cleanup moved classpath extension-property getter
resolution into `CallResolver`. The checker no longer performs the two-step
`physical_property_getter_name(property)` + `library_extension_callable(getter, recv, [], [])` query
itself; it asks the arg-dependent resolution layer for `resolve_extension_property_getter`. This keeps
property getter spelling and extension overload selection in one place and leaves the checker only
recording the selected callable for lowering.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test serialization_krusty_only_e2e -- --nocapture descriptor_element_names_extension_property
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-fifteenth-pass resolved-member shape cleanup removed duplicated return data from
`ResolvedMember`. The selected member already carries its platform physical return (`member.physical_ret`);
`ResolvedMember` now stores only the logical selected return plus the member handle. Lowering and
resolver consumers read the physical return from the member, so metadata-derived logical returns and
backend physical returns stay in one nested record instead of being copied into parallel fields.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test string_concat_append_overload_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-fourteenth-pass String index cleanup removed the direct `kotlin/String.get` external from
ordinary index-expression lowering. `"OK"[1]` now lowers by resolving `get(Int)` through the library
member metadata and emitting the selected owner/name/descriptor, the same path used by String foreach.
Array indexing remains on `kotlin/Array.get` because that is the IR array intrinsic, not a classpath
member. After this pass, `src/resolve.rs` and `src/ir_lower.rs` no longer spell
`kotlin/String.get` / `kotlin/String.length` as lowering targets.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test string_concat_append_overload_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-thirteenth-pass String `for` loop cleanup removed the remaining direct
`kotlin/String.length` / `kotlin/String.get` externals from foreach index-loop lowering. The String
loop path now asks the library/member resolver for `length` and `get(Int)` exactly like ordinary
member reads/calls; arrays keep their intrinsic `kotlin/Array.size/get` path because that is an IR
array operation, not classpath metadata. A focused e2e now pins `for (c in "OK")` so future agents do
not reintroduce the direct String externals while changing loop lowering.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test string_concat_append_overload_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twelfth-pass safe-call String/property cleanup removed another `kotlin/String.length`
lowering branch. Safe property fusion now derives the non-null semantic receiver type once
(`rty.non_null().kotlin_class_internal()`) and resolves the property through the same library member
path used by ordinary `recv.length`. Safe-call stdlib extensions use the non-null receiver inside the
null-checked branch, so `s?.uppercase()?.length ?: 0` keeps working without a String-specific lowering
case.

Guardrail: inline-only safe-call extensions are still not accepted when a callable-reference argument
is present. The corpus case `inlineClasses/boxReturnValueOnOverride/kt35234a.kt` exposes a separate
value-class callable-reference return bug; accepting it today produces a verifier failure. That shape
remains skipped until callable-reference/value-class return lowering is fixed, preserving the
zero-fail conformance contract.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test string_concat_append_overload_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-fifth-pass value-class emit gate audit recovered a broad skip introduced by the JVM emitter
applicability check. `jvm_can_emit` rejected every user value class whose underlying field erased to a
reference type; the value-class lowering and synthesized member paths already handle that shape, so the
gate was too coarse. The emitter now keeps only the remaining generic value-class guard, and
`tests/value_class_e2e.rs` pins the recovered `@JvmInline value class Id(val raw: String)` equality /
hashCode / toString runtime case.

The apparent historical "1840-ish" number in this file was not a zero-fail conformance metric:

```text
scanned: 7351 | krusty-compiled: 1840 | box()=OK: 1826 | skipped: 5511 | FAIL: 14
```

That run accepted 14 programs it could not execute correctly. The current comparable metric must use
the `FAIL: 0` invariant. After this pass the verified metric is:

```text
scanned: 7351 | krusty-compiled: 1715 | box()=OK: 1715 | skipped(unsupported): 5636 | FAIL: 0
```

Verification:

```sh
cargo fmt
cargo check --profile gate
./run-tests.sh --test value_class_e2e -- --nocapture value_class_reference_underlying_eq_hash_to_string_runs
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
cargo run --profile gate --bin survey -- target/cache/box-corpus/2.4.0/compiler/testData/codegen/box
```

Hundred-sixth-pass inline extension gate cleanup removed a stale checker-side shape restriction that
blocked `StringBuilder.appendLine` and other no-lambda `@InlineOnly` extensions on reference receivers.
The checker now accepts any selected no-function-parameter extension whose library metadata marks it
`MustInline`; the lowerer/emitter already own the real JVM splice route. This deleted the old
numeric/char-only receiver condition instead of adding a `StringBuilder.appendLine` branch.

That immediately exposed a backend access bug: optional splicing of public inline facade bridges can
copy package-private part-owner calls into user bytecode (`kotlin.test.AssertionsKt.assertTrue` forwarding
to `AssertionsKt__AssertionsKt`). The emitter now suppresses optional no-lambda splicing when the body is
a same-signature bridge to a different owner, while preserving forced `MustInline` splicing for private
stdlib helpers such as collection `plusAssign`.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1768 | box()=OK: 1768 | skipped(unsupported): 5583 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test stdlib_call_resolution_e2e -- --nocapture string_builder_append_line_resolves kotlin_test_assert_equals_resolves
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture range_contains_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-eighth-pass anonymous-object generic-scope cleanup removed another survey distortion and one
real accepted-case gap. Synthetic anonymous classes are parsed as hoisted file-level declarations, but
they can mention lexical function/class type parameters in supertypes and override signatures. The
parser now records the lexical type-parameter stack and attaches those names/bounds to synthesized
anonymous classes instead of letting the checker report `T` as unresolved after hoisting. This is a
generic hoisting fix, not a coroutine-helper branch: the survey's false `unresolved reference 'T'`
bucket dropped from 403 files to 27, exposing the real top buckets (`emit_all` bailouts, parse gaps,
`assertFailsWith`, coroutine intrinsics).

The broader acceptance exposed a JVM value-class bridge bug in
`inlineClasses/unboxGenericParameter/objectLiteral/resultAny.kt`: bridge parameters for external value
classes such as `kotlin.Result` were treated like boxed user value classes and emitted as
`checkcast Result; unbox-impl`. External value classes are already represented by their erased
underlying (`Object`) in krusty, so bridge parameter unboxing now applies only to user value classes.
The corpus regression set pins both the generic anonymous-object scope and the `Result` bridge case.
Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1769 | box()=OK: 1769 | skipped(unsupported): 5582 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture anonymous_object_keeps_enclosing_function_type_params
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture result_value_class_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-ninth-pass generic value-class gate cleanup removed a stale broad JVM emitter rejection:
`jvm_can_emit` used to decline every value class with type parameters even though the JVM
`value_classes` lowering pass already models generic value classes with an erased `Object` underlying
and bound-derived nullability. Removing that blanket check collapsed the survey's top
`emit_all bailed` bucket from 134 files to 10 and converted 120 additional corpus files into verified
runtime passes.

The broader acceptance exposed two precise backend boundaries. One was fixed generically:
Object/Comparable contract methods (`compareTo`, `equals`, `hashCode`, `toString`) now keep their
fixed return types before expression-body inference, and the checker no longer patches `toString()` to
erased `Any/Object` when a generic body returns `T`. This fixes
`inlineClasses/toStringCallingPrivateFunGeneric.kt` as a real `toString(): String` override. The other
boundary remains modeled as a precise JVM applicability guard: generic value classes whose erased
underlying field is `Comparable` still need primitive-to-reference adaptation before unboxing and are
declined at the IR shape level instead of by source-name skip or a blanket generic-value-class bail.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1889 | box()=OK: 1889 | skipped(unsupported): 5462 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture generic_value_class_corpus_cases_box_ok
cargo run --profile gate --bin survey -- target/cache/box-corpus/2.4.0/compiler/testData/codegen/box --samples "emit_all bailed"
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-third-pass metadata return cleanup removed the remaining split lookup of metadata return class
and metadata return nullability. `Classpath::metadata_return()` now returns one `MetadataReturn` record
from the same decoded `MetaCallable`, and JVM library resolution consumes that single record. This
prevents recombining a return class from one overload with a nullable bit from another and removes the
old `metadata_return_type` / `metadata_return_nullable` public API pair from production.

Hundred-fourth-pass metadata overload cleanup removed the separate
`package_lambda_return_overloads` metadata decoder. The `@OverloadResolutionByLambdaReturnType` cache
now derives its `(kotlin name -> JVM overloads)` view from the same facade-merged `meta_functions()`
decode used by the rest of classpath metadata. This keeps `sumOf`/lambda-return overload selection on
the unified metadata path instead of parsing package metadata a second way.

Current conformance metric is unchanged:

```text
scanned: 7351 | krusty-compiled: 1617 | box()=OK: 1617 | skipped(unsupported): 5734 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test metadata_return_types -- --nocapture
./run-tests.sh --test metadata_kept_params -- --nocapture
./run-tests.sh --test resolver_regression_e2e -- --nocapture lambda_return_overload_stays_separate_from_normal_inline_hofs
./run-tests.sh --test inline_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Note: `./run-tests.sh --test feature_box_e2e -- --nocapture` currently fails at
`ValueClassEqHashStr`, outside this metadata-overload path; the targeted `sumOf` resolver regression
and full conformance gate pass.

Hundred-second-pass metadata projection cleanup collapsed the classpath `@Metadata` cache's parallel
maps into one callable record. `ClassMeta` now stores a `MetaCallable` vector plus Kotlin-name/JVM-name
indexes, so return class, extension receiver, return nullability, source parameter types, source
parameter names/default flags, and receiver-lambda annotations stay attached to the same overload.
The public `metadata_*` APIs still project the same answers, but the internal data shape no longer has
separate `return_types`, `receivers`, `return_nullable`, and `overload_params` maps that can drift by
name or overload ordering. The old production-unused metadata projection helper functions
(`return_types`, `receivers`, `return_nullable`) were deleted too; tests now assert directly on decoded
`MetaFn` records.

Current conformance metric is unchanged:

```text
scanned: 7351 | krusty-compiled: 1617 | box()=OK: 1617 | skipped(unsupported): 5734 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test metadata_return_types -- --nocapture
./run-tests.sh --test metadata_kept_params -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-first-pass call-resolution identity cleanup removed one duplicated built-in type mapping from
`call_resolver`. The resolver needs Kotlin class identity for subtype/member lookup, but it should not
carry an ad hoc `Ty -> "kotlin/..."` table next to every lookup site. `Ty::kotlin_class_internal()`
now owns that source-level identity (`Int` -> `kotlin/Int`, `String` -> `kotlin/String`, user `Obj`
internals unchanged, nullable/type-parameter forms delegated), and call resolution uses it for
reference subtype checks and instance member selection. This reduces one local hardcode table without
moving JVM descriptor logic into core.

Next larger cleanup target from the same audit: classpath metadata is decoded once but projected into
parallel maps (`return_types`, `receivers`, `return_nullable`, kept params, overloads) and rejoined by
name later. That should become one keyed metadata callable record so return nullability, source return
type, receiver type, and parameter metadata cannot drift.

Current conformance metric is unchanged:

```text
scanned: 7351 | krusty-compiled: 1617 | box()=OK: 1617 | skipped(unsupported): 5734 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --lib types -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Eighty-ninth-pass discarded-coercion cleanup removed the broad `buildMap` conformance skip after fixing
the real emitter bug behind it. Statement-position `ImplicitCoercion` now emits and discards the operand's
physical value directly instead of performing value-context boxing/unboxing first. This keeps side effects
while avoiding the invalid `Map.put(...): Object?` -> `Int` unbox when the previous value is unused.
Unsupported conformance predicates and unsupported stdlib value-class emit checks were also consolidated
into named lists, so new gates are auditable instead of scattered string conditions. A follow-up narrowed
the `Result` gate to preserve simple `Result.success(...).getOrThrow()` support while the remaining
inline/value-class cases were modeled. Historical conformance metric from that pass:

```text
scanned: 7351 | krusty-compiled: 1559 | box()=OK: 1559 | skipped(unsupported): 5792 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test collection_members_e2e -- --nocapture
./run-tests.sh --test result_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninetieth-pass value-class metadata member cleanup removed the `.getOrNull()` conformance source gate
without adding a `Result` special case. `LibraryMember` now carries metadata return nullability, a
separate physical method name, and inline-ness; value-class metadata members such as
`Result.getOrNull()` resolve by Kotlin name but lower to the static `getOrNull-impl(receiver)` body and
must-inline private implementations are spliced instead of invoked. The checker's structural equality
rule was narrowed to the safe generic shape (`Any?/Any ==` a boxable non-floating value) so `getOrNull()
== 42` works without reopening boxed `Double`/`Float` equality miscompiles.

The bytecode inliner also now synthesizes descriptor-based branch frames for small branchy inline bodies
that have no source `StackMapTable`, which covers `getOrNull-impl`'s `if failure then null else value`
shape and removes another name-specific workaround. Current conformance metric remains:

```text
scanned: 7351 | krusty-compiled: 1559 | box()=OK: 1559 | skipped(unsupported): 5792 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
cargo test --profile gate jvm::inline -- --nocapture
./run-tests.sh --test metadata_reader_e2e -- --nocapture result_get_or_null_resolves_as_nullable_metadata_member
./run-tests.sh --test result_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninety-first-pass `Result` gate removal completed the remaining generic `Result<T>` corpus support that
the earlier pass still excluded. Expression-body return inference now has an early top-level prepass for
top-level and extension functions, and same-file member-call resolution consults newly inferred class
method returns before the final `SymbolTable` writeback. This removes declaration-order dependence in
`class C : I<Result<Any?>> { override fun foo(x: Result<Any?>) = x.getOrNullNoinline() }` followed by
`fun test() = C().foo(...)`.

The JVM value-class paths were aligned as well: implicit-`this` library member lowering now uses the same
descriptor-based static-receiver check as qualified member lowering, so `getOrNull()` inside a
`Result<T>` extension lowers to the static `getOrNull-impl(receiver)` path. Return-tail value-class
boxing now only treats actual entries in the value-class underlying map as value classes and keeps
classpath/external value classes such as `Result` in their erased representation instead of synthesizing
nonexistent `box-impl` calls. Value-class bridge dedupe runs after JVM erasure so a real method and a
bridge with the same final descriptor do not duplicate.

The same conformance sweep exposed an unrelated inner-class constructor slot bug: `super(...)` argument
lowering now accounts for the synthetic `this$0` parameter before source constructor parameters, fixing
`classes/inner/properSuperLinking.kt`. Current conformance metric:

```text
scanned: 7351 | krusty-compiled: 1585 | box()=OK: 1585 | skipped(unsupported): 5766 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test collection_members_e2e -- --nocapture build_map_put_statement_discards_nullable_previous_value
./run-tests.sh --test metadata_reader_e2e -- --nocapture result_get_or_null_resolves_as_nullable_metadata_member
./run-tests.sh --test result_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture inner_constructor_corpus_cases_box_ok result_value_class_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninety-second-pass module symbol boundary cleanup removed JVM descriptor synthesis from
`ModuleSymbols`. The current compilation's own declarations now expose only semantic params/returns and
`Origin::Module`; the lowerer derives same-file and cross-file JVM call shapes from IR / `SymbolTable`
data at the backend boundary. This eliminates another core→JVM dependency and changes the unit tests to
assert semantic callable shape instead of descriptor strings. Current conformance metric is unchanged:

```text
scanned: 7351 | krusty-compiled: 1585 | box()=OK: 1585 | skipped(unsupported): 5766 | FAIL: 0
```

Verification:

```sh
cargo check --profile gate
cargo test --profile gate module_symbols -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninety-fifth-pass value-class metadata cleanup removed a resolver-side unsigned exception from
no-lambda inline extension typing. That path now asks the federated library source for
`value_underlying(ret)` instead of checking `ret.is_unsigned()` and separately probing object metadata.
The JVM provider already owns the builtin unsigned-underlying facts (`UInt`/`ULong`) and ordinary
classpath value-class metadata (`Result`, user jars), so resolver no longer needs a local table of
which semantic returns are JVM-erased value classes. Current conformance metric is unchanged:

```text
scanned: 7351 | krusty-compiled: 1585 | box()=OK: 1585 | skipped(unsupported): 5766 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_metadata_return_blocks_unsupported_inline_splice unsigned_integral_conversions_resolve_from_metadata
./run-tests.sh --test unsigned_ext_e2e -- --nocapture
./run-tests.sh --test result_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninety-third-pass value-class member dispatch cleanup moved the remaining descriptor-explicit receiver
decision out of common lowering. The shared lowerer now keeps classpath members as semantic
`Callee::Virtual` calls with a dispatch receiver; it no longer parses JVM descriptors or carries a
JVM-shaped receiver-passing flag. The JVM emitter, which already owns descriptors and inline
splicing, recognizes the physical one-extra-parameter shape, attempts to splice the private inline body
first, and only then emits the JVM physical call. This keeps `Result.getOrNull-impl` legal without
leaking its JVM implementation model into resolver/lowering data. Current conformance metric is
unchanged:

```text
scanned: 7351 | krusty-compiled: 1585 | box()=OK: 1585 | skipped(unsupported): 5766 | FAIL: 0
```

Verification:

```sh
cargo check --profile gate
./run-tests.sh --test result_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture result_value_class_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninety-fourth-pass classpath parameter-shape cleanup removed another descriptor parser from common
lowering. Top-level JVM library callables now use the JVM provider's existing exact field-param parser,
so descriptor params `B`/`S` and `[B`/`[S` are exposed as Kotlin-facing `Byte`/`Short` parameter
shapes instead of being collapsed to `Int` at the library boundary. The lowerer still conservatively
skips unresolved byte/short argument adaptation, but it now makes that decision from the selected
callable's semantic params rather than by scanning a JVM method descriptor. Current conformance metric
is unchanged:

```text
scanned: 7351 | krusty-compiled: 1585 | box()=OK: 1585 | skipped(unsupported): 5766 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninety-sixth-pass range typing consolidation removed three local copies of the same
`Byte|Short|Int` range rule. `Ty::is_int_range_operand()` now owns the Kotlin semantic fact that
`Byte`/`Short`/`Int` operands select `IntRange`, while any mix with `Long` selects `LongRange`.
Literal inference, full checking, and IR lowering all call the same helper, so future range support no
longer has to update three hand-written closures in lockstep. Current conformance metric is unchanged:

```text
scanned: 7351 | krusty-compiled: 1585 | box()=OK: 1585 | skipped(unsupported): 5766 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture range_to_corpus_cases_box_ok range_small_int_corpus_cases_box_ok unsigned_compare_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninety-seventh-pass numeric/char shape consolidation removed duplicated primitive-member and inline
extension receiver checks. `Ty::is_numeric_or_char()` now owns the semantic shape for signed numeric
types plus `Char` (excluding unsigned value classes), replacing repeated long `matches!` lists and
`is_numeric() || Char` checks in resolver paths. This keeps builtin operator-method resolution and
no-lambda inline extension admission aligned without another local type table. Current conformance
metric is unchanged:

```text
scanned: 7351 | krusty-compiled: 1585 | box()=OK: 1585 | skipped(unsupported): 5766 | FAIL: 0
```

Verification:

```sh
cargo fmt
cargo check --profile gate
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_metadata_return_blocks_unsupported_inline_splice unsigned_integral_conversions_resolve_from_metadata
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture primitive_member_corpus_cases_box_ok primitive_inc_dec_corpus_cases_box_ok
./run-tests.sh --test resolver_regression_e2e -- --nocapture primitive_builtin_infix_extension_source_form_matters
./run-tests.sh --test safe_call_prim_intrinsic_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninety-eighth-pass property-member resolution cleanup removed three copies of library property
fallback logic from IR lowering. Lowering now uses `call_resolver::resolve_property_member`, the same
semantic-property plus provider-owned physical-getter path used by checking, instead of open-coding
`property_getter_name` fallback and `Unit`/`Error` return filters in each call site. The direct
property path preserves `ResolvedMember.physical_ret` when coercing erased generic reads; this keeps
generic properties such as `Pair.second` checkcast/unboxed correctly. Current conformance metric is
unchanged:

```text
scanned: 7351 | krusty-compiled: 1585 | box()=OK: 1585 | skipped(unsupported): 5766 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test collection_members_e2e -- --nocapture
./run-tests.sh --test extension_property_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture collection_mapped_corpus_cases_box_ok primitive_member_corpus_cases_box_ok string_get_corpus_cases_box_ok
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture collection_mapped_corpus_cases_box_ok range_to_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Ninety-ninth-pass read-result filtering cleanup consolidated the remaining `Unit`/`Error` return
filters used by property-like member reads. `Ty::is_read_value_result()` now owns the current semantic
rule for zero-arg value reads, and resolver/call-resolver paths use that helper instead of repeating
`matches!(ret, Unit | Error)` checks. This keeps property selection, extension-property fallback, and
raw member-read fallback aligned while preserving the existing behavior. Current conformance metric is
unchanged:

```text
scanned: 7351 | krusty-compiled: 1585 | box()=OK: 1585 | skipped(unsupported): 5766 | FAIL: 0
```

Verification:

```sh
cargo fmt
cargo check --profile gate
./run-tests.sh --test collection_members_e2e -- --nocapture
./run-tests.sh --test extension_property_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture collection_mapped_corpus_cases_box_ok primitive_member_corpus_cases_box_ok string_get_corpus_cases_box_ok range_to_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundredth-pass conformance gate audit recovered value-class coverage that the broad
`WORKS_WHEN_VALUE_CLASS` skip was hiding. Removing that directive gate exposed one real backend bug:
`inlineClasses/overrideReturnNothing.kt` (value-class interface members overridden by `Nothing` /
`Nothing?`). The fix keeps explicit nullable property IR types boxed, emits verifier-safe bridges for
bottom-returning overrides, and gives value-class-return bridges the JVM name expected by the
interface (functions mangled, property getters unmangled). Current conformance metric improves to:

```text
scanned: 7351 | krusty-compiled: 1617 | box()=OK: 1617 | skipped(unsupported): 5734 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
KRUSTY_KOTLIN_BOX_DIR="$PWD/target/cache/box-corpus/2.4.0/compiler/testData/codegen/box" ./run-tests.sh --test box_corpus_regression_e2e -- --nocapture value_class_nothing_override_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-seventh-pass conformance metric audit initially treated the apparent `1840/1842 -> 1585` drop
as a mixed-metric comparison. The fuller `target/ir_conformance_trend.csv` history shows two distinct
facts. First, there were real zero-fail `1842/1842/0` rows before the cleanup. Second, the later
1840-ish run was non-green:

```text
scanned: 7351 | krusty-compiled: 1840 | box()=OK: 1826 | skipped: 5511 | FAIL: 14
```

That non-green row is not comparable to a `FAIL: 0` gate. The real temporary cliff happened in the
Eighty-eighth-pass conformance-safety cleanup: the gate stopped counting unsupported shapes as compiled
support and started skipping builder-inference directives, JS-runtime-only files, generic/advanced
`Result<T>` shapes, unsupported `UByte`/`UShort` value classes, and `WORKS_WHEN_VALUE_CLASS` advanced
value-class cases. The historical 1585 entries are zero-fail checkpoints after that stricter
skip-over-miscompile policy. The current verified zero-fail conformance metric is:

```text
scanned: 7351 | krusty-compiled: 1768 | box()=OK: 1768 | skipped(unsupported): 5583 | FAIL: 0
```

Verification:

```sh
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-tenth-pass kotlin-test inline/default cleanup recovered the `assertFailsWith` bucket. The loss
was not a missing `kotlin-test.jar`: `assertFailsWith<T> { ... }` is a private/defaulted/reified
`@InlineOnly` top-level function in a package-private implementation class. Three generic gaps hid it:
the classpath static index ignored non-public implementation classes, `$default` inline metadata was
queried under the synthetic JVM name instead of the source metadata name, and top-level default-call
resolution/checking assumed provided arguments were a prefix, so a trailing lambda was compared to the
defaulted `message: String?` slot.

The fix keeps the abstraction boundary intact: the provider exposes non-public non-callable statics as
splice-only candidates, resolver/default argument mapping handles a trailing lambda in the last source
parameter, and the target runtime identifies the selected callable as the defaulted reified
assert-fails helper. Common lowering then realizes that semantic shape as ordinary IR
(`try { block(); throw AssertionError(...) } catch (e: Expected) { e }`) using provider-owned runtime
constructors instead of emitting an illegal package-private call or forcing the bytecode splicer to
handle exception-table reified bodies in one step. The survey now reports `inline_bail_reason()` so
future emit buckets identify the selected callee instead of collapsing under `emit_all bailed`.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test stdlib_call_resolution_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture assert_fails_with_corpus_cases_box_ok
cargo run --profile gate --bin survey -- target/cache/box-corpus/2.4.0/compiler/testData/codegen/box --samples "assertFailsWith"
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twenty-fifth-pass unsigned value-class runtime cleanup moved the remaining executable
`box-impl`/`unbox-impl` spelling for unsigned value classes out of common IR lowering. `ir_lower` now
requests semantic `RuntimeOp::UnsignedBox` / `RuntimeOp::UnsignedUnbox` helpers; `JvmLibraries`
returns the JVM owner, method name, and descriptor. This keeps common lowering from manufacturing JVM
value-class synthetic names while preserving the current zero-fail conformance metric.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test unsigned_toplevel_e2e -- --nocapture
./run-tests.sh --test value_class_e2e -- --nocapture sized_array_of_value_class_uses_provider_value_underlying
./run-tests.sh --test resolver_regression_e2e -- --nocapture unsigned_binary_operators_use_library_type_identity unsigned_integral_conversions_resolve_from_metadata
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twenty-sixth-pass range construction cleanup moved executable JVM range details out of common
IR lowering. `ir_lower` now asks `TargetRuntime::range_construction(lo, hi)` for the semantic element
type, `..` constructor plan, and optional `..<` helper; `JvmLibraries` owns the range runtime class,
constructor descriptor, `DefaultConstructorMarker` null slot, and `RangesKt.until` facade callable.
Common lowering still chooses source syntax (`..` vs `..<`) and lowers/coerces operands, but no longer
formats JVM descriptors or names range helper facades itself.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture range_to_corpus_cases_box_ok range_small_int_corpus_cases_box_ok range_contains_corpus_cases_box_ok unsigned_compare_corpus_cases_box_ok
./run-tests.sh --test unsigned_toplevel_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twenty-seventh-pass function erasure cleanup removed the remaining JVM function-interface
formatter from core `Ty`. Resolver erased-signature keys now represent function types as
`ErasedTypeKey::Function(arity)` instead of building `kotlin/jvm/functions/FunctionN`; nested function
keys that must be embedded in another erased `Ty` use the Kotlin semantic `kotlin/FunctionN` name. JVM
function representation stays in the JVM provider/emitter paths, while core overload/bridge checks keep
their equality semantics without platform class names.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test generic_hof_method_check -- --nocapture
./run-tests.sh --test callable_ref_e2e -- --nocapture
./run-tests.sh --test stdlib_call_resolution_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twenty-eighth-pass coroutine runtime cleanup moved the physical `startCoroutine` helper call
out of common member-call lowering. The lowerer still recognizes the semantic coroutine intrinsic and
lowers the receiver/completion arguments, but it now asks `RuntimeOp::StartCoroutine` for the callable;
`JvmLibraries` owns the `ContinuationKt` owner and JVM `Function1, Continuation -> Unit` descriptor.
This removes another executable `kotlin/jvm/functions` descriptor from common lowering.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test coroutine_intrinsics_e2e -- --nocapture
./run-tests.sh --test suspend_e2e -- --nocapture suspend_function_type_lowers_to_function1_continuation leaf_suspend_lambda_creates_and_invokes suspend_lambda_with_parameter_runs
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-twenty-ninth-pass coroutine helper cleanup moved the remaining coroutine helper callables out
of common suspend-lambda lowering. `throwOnFailure(result)` and `COROUTINE_SUSPENDED` reads now go
through `RuntimeOp::ThrowOnFailure` / `RuntimeOp::CoroutineSuspended`; `JvmLibraries` owns
`ResultKt.throwOnFailure`, `IntrinsicsKt.getCOROUTINE_SUSPENDED`, and their JVM descriptors. Common
lowering keeps only the semantic state-machine placement of those helpers.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test coroutine_intrinsics_e2e -- --nocapture
./run-tests.sh --test suspend_e2e -- --nocapture leaf_suspend_lambda_creates_and_invokes suspend_lambda_two_suspensions_runs suspend_lambda_with_internal_suspension_runs
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-thirtieth-pass suspend CPS descriptor cleanup moved the last hand-built suspend-call CPS
descriptor from common suspend-lambda lowering into the target runtime. The inline lambda state-machine
path still decides when a logical suspend callee needs the current continuation threaded in, but it now
asks `TargetRuntime::suspend_cps_descriptor` for the physical descriptor; `JvmLibraries` owns the JVM
`Continuation` parameter and erased `Object` return spelling.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test suspend_e2e -- --nocapture suspend_lambda_with_internal_suspension_runs suspend_lambda_two_suspensions_runs suspend_fun_calls_cross_file_suspend_fun suspend_fun_calls_classpath_suspend_fun
./run-tests.sh --test coroutine_intrinsics_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-thirty-first-pass enum accessor cleanup moved enum built-in accessor descriptors out of common
member-property lowering. The lowerer still verifies that the receiver is a user enum and dispatches on
the enum's static owner, but `TargetRuntime::enum_member_accessor` now supplies the physical accessor
name and descriptor. `JvmLibraries` owns `ordinal()I` and `name()String`, removing those JVM descriptor
literals from common lowering.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test session_subsystems_e2e -- --nocapture enum_entries_and_values
./run-tests.sh --test enum_entries_e2e -- --nocapture
./run-tests.sh --test enum_class_signature_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-thirty-second-pass singleton field cleanup moved object and companion singleton field shapes out
of common lowering. `TargetRuntime` now exposes `object_instance_field` and
`companion_instance_field`, returning a provider-owned `PlatformField` with owner/name/descriptor.
Classpath object values, nested object values, companion-as-value reads, value-class companion calls,
and object-valued constructor defaults now use those hooks instead of spelling JVM `INSTANCE`,
`Companion`, or `L...;` field descriptors in common lowering.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test classpath_object_value_e2e -- --nocapture
./run-tests.sh --test companion_supertype_e2e -- --nocapture
./run-tests.sh --test object_default_ctor_arg_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-thirty-third-pass function invoke cleanup removed a one-off JVM `Function0.invoke` construction
from the `assertFailsWith<T> { ... }` semantic lowering. That path now invokes the lambda via
`IrExpr::InvokeFunction`, the same backend-agnostic function-value IR used by ordinary `f()` calls,
instead of spelling the `invoke` method, `Function0` owner, and erased `Any` descriptor in common
lowering.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test stdlib_call_resolution_e2e -- --nocapture kotlin_test_assert_fails_with_resolves kotlin_test_assert_fails_with_default_is_inline_only_callable
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture assert_fails_with_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-thirty-fourth-pass platform field consumption cleanup centralized the common lowerer's
conversion from provider-owned `PlatformField` into `IrExpr::ExternalStaticField`. Classpath object
values, nested object values, companion-as-value reads, value-class companion calls, and object-valued
constructor defaults now all consume provider singleton-field facts through one helper instead of
re-expanding owner/name/descriptor blocks at each site.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test classpath_object_value_e2e -- --nocapture
./run-tests.sh --test companion_supertype_e2e -- --nocapture companion_used_as_its_interface_value companion_extends_default_param_base_runs
./run-tests.sh --test object_default_ctor_arg_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-thirty-fifth-pass reference helper cleanup removed the duplicated boxed-primitive spelling
table from `ir_type_is_reference`. Lowering now asks `Ty::unboxed_primitive()` to decide whether an
object-shaped IR type is one of the boxed primitive spellings, keeping that fact in the type helper
instead of repeating `kotlin/Int`, `kotlin/Long`, and the other primitive names in the lowerer.

Important failed experiment: do not change nullable primitive `ty_to_ir(Int?)` to
`inner.boxed_ref()` without first moving the physical wrapper decision behind an explicit platform
hook. The current JVM backend expects nullable primitive IR slots to use wrapper owners such as
`java/lang/Integer`; changing them to semantic `kotlin/Int` produced verifier failures in
`nullable_primitive_box_e2e`.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 1953 | box()=OK: 1953 | skipped(unsupported): 5398 | FAIL: 0
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test nullable_primitive_box_e2e -- --nocapture
./run-tests.sh --test generic_inferred_return_e2e -- --nocapture
./run-tests.sh --test generic_signature_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
```

Hundred-thirty-sixth-pass descriptor ownership cleanup moved common lowering's JVM descriptor spelling
behind `TargetRuntime`. The platform now provides source-level `type_descriptor`/`method_descriptor`
hooks and a separate `ir_type_descriptor` hook for already-lowered IR types; the JVM implementation
keeps annotation accessor ABI correct by mapping IR spellings such as `Obj("kotlin/Int")` back to their
physical primitive descriptors internally. Common lowering no longer imports JVM descriptor helpers or
the JVM emitter for annotation member calls.

Hundred-thirty-seventh-pass unsigned exclusive range cleanup turned a survey first-failure category
into a provider fact. `RangeConstruction::until` now includes the JVM stdlib's unsigned helpers
(`URangesKt.until-J1ME1BU` and `URangesKt.until-eb3DHEI`), so the existing generic `RangeTo` lowering
supports `UInt`/`ULong` `until` and `..<` without adding corpus-specific cases to common lowering.
This removed the `lower: expr RangeTo` survey bucket and raised codegen/box conformance by 71 files.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 2024 | box()=OK: 2024 | skipped(unsupported): 5327 | FAIL: 0
```

Major remaining first-failure buckets from `cargo run --profile gate --bin survey -- target/cache/box-corpus/2.4.0/compiler/testData/codegen/box`:

```text
98 parse: expected an expression
83 parse: expected ')'
82 unresolved function 'suspendCoroutine'
77 parse: expected a top-level declaration
74 lower: deep
73 lower: call suspendCoroutineUninterceptedOrReturn
63 unresolved method 'apply' on 'Buildee'
61 callable references are not supported
```

`RangeTo` before/after survey sample:

```text
before: Category: lower: expr RangeTo (71 files)
after:  Scanned: 5694  Compiled: 2035  Skip-errors: 3659
```

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test annotation_instantiation_e2e -- --nocapture
./run-tests.sh --test range_step_e2e -- --nocapture
./run-tests.sh --test resolver_regression_e2e -- --nocapture
./run-tests.sh --test box_corpus_regression_e2e -- --nocapture unsigned_until_range_corpus_cases_box_ok
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
cargo run --profile gate --bin survey -- target/cache/box-corpus/2.4.0/compiler/testData/codegen/box --samples "lower: expr RangeTo"
```

Hundred-thirty-eighth-pass receiver-scope function-value cleanup fixed the generic stdlib inline path
for calls such as `Buildee<T>().apply(instructions)`, where `instructions` is a receiver-function value
rather than a literal receiver lambda. Literal `apply { ... }` still uses the receiver-aware scope
lowering; the non-lambda function-value shape now resolves through the provider-backed inline extension
route. The rule deliberately preserves the older safe `@InlineOnly` no-function-parameter route and does
not admit arbitrary function-parameter inline calls: a broad version accepted `let(::f)` value-class
callable-reference cases and produced 15 `NoSuchMethodError` conformance failures, so those continue to
skip until value-class callable-reference bridge/erasure is modeled.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 2024 | box()=OK: 2024 | skipped(unsupported): 5327 | FAIL: 0
```

Survey result for the targeted first-failure bucket:

```text
before: Category: other: unresolved method 'apply' on 'Buildee' (63 files)
after:  Scanned: 5694  Compiled: 2035  Skip-errors: 3659
next:   Category: other: unresolved method 'instructions' on 'Buildee' (3 files)
```

This does not raise `box()=OK` yet: the affected PCLA files now move past `apply(instructions)` and hit
later builder-inference/type-flow blockers. The next metric-moving work should target those deeper PCLA
constraints or one of the remaining large buckets (`parse: expected an expression`, suspend coroutine
calls, callable references).

Verification:

```sh
cargo fmt --check
cargo check --profile gate
./run-tests.sh --test stdlib_call_resolution_e2e -- --nocapture receiver_scope_function_accepts_function_value_argument
./run-tests.sh --test scope_function_value_arg_e2e -- --nocapture
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
cargo run --profile gate --bin survey -- target/cache/box-corpus/2.4.0/compiler/testData/codegen/box --samples "unresolved method 'apply' on 'Buildee'"
cargo run --profile gate --bin survey -- target/cache/box-corpus/2.4.0/compiler/testData/codegen/box --samples "unresolved method 'instructions' on 'Buildee'"
```

Hundred-thirty-ninth-pass current-state audit after the platform-provider cleanup found the next
systemic blocker: nullable generic return semantics were still not represented as first-class overload
data. The first cleanup removed the most obvious resolver hardcodes: `takeIf`/`takeUnless` now use
stdlib `@Metadata` nullability even when the return has no concrete class (`T?`), and `MutableMap.put`
nullability is supplied by the JVM library provider for mapped Kotlin/JVM map members. The generic
resolver no longer matches these JVM descriptors:

```text
takeIf/takeUnless + (Object, Function1)Object => T?
put + (Object, Object)Object => V?
```

That is evidence that the metadata/generic-signature model still loses nullability on type-variable
returns (`T?`, `V?`). When lost, lowering sees a logical `Int`/`String` instead of `Int?`/`String?` and
either unboxes null (`takeIf`, `Map.put`) or returns raw `Object` from a lambda that declares `String`.
The runtime failures were:

```text
TakeIfNullableResultKt: NullPointerException from unboxing null Integer
buildMap/kt64066.kt: NullPointerException from Map.put returning null previous value
withoutAnnotation.kt: VerifyError, lambda returning Object where String was declared
```

The remaining local fixes are still compatibility patches, not the desired architecture:

- `Lower::coerce_erased_call_result` now correctly unboxes erased non-null scalar returns at use sites.
- Lambda synthesis now casts reference returns to the lambda signature return type before `areturn`.

The broader generic fix should still carry return nullability in `GSig`, not add more descriptor
exceptions. `GenericSig.ret` currently stores only a type tree, so `T` and `T?` collapse unless the
provider has a side-channel `FunctionInfo.ret_nullable`. Replace it with a `GType { kind, nullable }`
(or equivalent) and propagate that through `gsig_to_ty` / return binding:

```rust
pub struct GType {
    pub kind: GKind,
    pub nullable: bool,
}
```

Then `FunctionInfo`/`LibraryCallable` can expose the selected return as:

```rust
logical_ret: Ty,
logical_ret_nullable: bool,
physical_ret: Ty,
```

or a single `ReturnShape { logical: Ty, nullable: bool, physical: Ty }`. Remaining deletion targets:

- stop using JVM descriptors to recover source-level nullability;
- make lambda return coercion consume `ReturnShape` instead of guessing from `Ty`;
- add a grep gate that rejects new descriptor strings in `call_resolver.rs`.

This is a high-value deletion target because it generalizes beyond the three failing cases. Any stdlib
or Java/Kotlin metadata member returning `T?` should then work through the same path.

Current verified conformance metric:

```text
scanned: 7351 | krusty-compiled: 2024 | box()=OK: 2024 | skipped(unsupported): 5327 | FAIL: 0
```

Verification:

```sh
just clippy-baseline-check
./run-tests.sh
./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture
git push origin master
```

The pushed commit after rebasing onto `origin/master` was:

```text
bdc7422 compiler: move platform facts behind providers
```

Hundred-fortieth-pass return-shape follow-up tightened the consumer side of the metadata cleanup. After
`FunctionInfo.ret_nullable` started carrying top-level metadata nullability, the top-level call paths
still had four separate copies of "bind generic return, apply metadata return class" and none applied
the nullable bit. `CallResolver` now has one `selected_return_type(ret_class, ret_nullable, fallback)`
helper used by normal top-level calls, default-call lowering, inline-only top-level calls, and extension
default calls. A unit regression pins a top-level callable whose metadata says `String?` so the selected
logical return becomes `Ty::nullable(Ty::String)` while the physical return stays `String`.

This is still a stepping stone, not the final architecture. The next deletion target is to replace the
parallel `ret`, `ret_nullable`, `ret_class`, and `physical_ret` fields with a single selected
`ReturnShape`. That should remove the remaining ad hoc call-site return assembly and make nullable
generic returns impossible to partially apply.
