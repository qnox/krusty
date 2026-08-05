# Kotlin contracts — IR, metadata wire format, and call-site application

Landed in PR #430. This note records the shared contract IR and the `@kotlin.Metadata` wire
details that were reverse-engineered the hard way (enum orders that are NOT the Kotlin
declaration order, the TypeTable indirection, the value-parameter off-by-one). Companion to
`METADATA_NOTES.md`, which covers the base proto layout and string table.

## 1. Pipeline overview

One IR — `src/contracts.rs` — is shared by all four stages so a contract means the same thing
no matter where it came from:

```
source contract { … } block          @kotlin.Metadata on a classfile
        │                                     │
        ▼ decode_source            Function.contract (field 32) decode
┌───────────────────────────────────────────────────┐
│  contracts::Contract { effects: Vec<Effect> }     │
└───────────────────────────────────────────────────┘
        │ rides Signature.contract (resolve.rs) cross-file
        ▼                                     ▲
checker call-site application         metadata emission
(contract_for_call,                   (metadata/builder.rs —
 conclusion_narrowings,                exact mirror of the reader)
 contract_condition_narrowings)
```

- `Effect::Returns(ReturnsValue)` — `returns()` / `returnsNotNull()` / `returns(true|false|null)`.
- `Effect::ConditionalReturns { returns, conclusion }` — `returns(X) implies <condition>`.
- `Effect::CallsInPlace { param, kind }` — `callsInPlace(lambda, KIND)`.
- `Condition`: `IsNull` / `IsType` / `BoolParam` / `Const` / `And` / `Or`.
- `ConditionType::{Source(TypeRef), Metadata(Ty)}` — source contracts carry the unresolved AST
  reference (the checker resolves it at the call site, against the call's type arguments);
  metadata contracts carry the decoded semantic type. `Contract::with_resolved_types` bridges
  the two for emission.

Applied at call sites today: `returns(…) implies …` conclusions (`ConditionalReturns`) drive
smart casts; unconditional `Returns` effects and `callsInPlace` are decoded and encoded but NOT
yet consumed (krusty's parser synthesizes a type default for `val x: T` without an initializer
instead of enforcing assign-once).

## 2. Wire format (`core/metadata/src/metadata.proto`)

`Function.contract` = **field 32** → `Contract` message; `Contract.effect` = 1 (repeated `Effect`).

### `Effect`

| field | meaning |
|---|---|
| 1 | `effect_type`: RETURNS_CONSTANT = 0 / CALLS = 1 / RETURNS_NOT_NULL = 2 / RETURNS_RESULT_OF = 3 (not modeled) |
| 2 | `effect_constructor_argument` (repeated `Expression`) |
| 3 | `conclusion_of_conditional_effect` (`Expression`) |
| 4 | `kind` (`InvocationKind`) |
| 5 | `condition_kind` |

There is **no conditional effect type**: field 3 being present (with field 5 absent, i.e. the
default CONCLUSION_CONDITION = 0) turns the returns-effect into `<returns> implies <conclusion>`.
RETURNS_CONDITION / HOLDS_IN forms are not modeled.

**`InvocationKind` wire order is NOT the Kotlin declaration order** — verified against
kotlin-stdlib's `run` (EXACTLY_ONCE): `AT_MOST_ONCE = 0 / EXACTLY_ONCE = 1 / AT_LEAST_ONCE = 2`.
The mapping lives in exactly one place: `InvocationKind::from_wire`/`to_wire`
(`src/contracts.rs`). `Unknown` (a kindless `callsInPlace(x)`) has no wire form — emit OMITS
field 4, and the reader defaults a missing kind to wire 0, so a kindless effect reads back as
`AtMostOnce` (the kindless form does NOT round-trip).

### `Expression` (conclusions / CALLS argument)

| field | meaning |
|---|---|
| 1 | `flags`: bit 0 = negated, bit 1 = null-check predicate |
| 2 | `value_parameter_reference`: **0 = extension receiver, else the 1-based value-parameter index** — the off-by-one lives only in `ParamRef::from_wire`/`to_wire` |
| 3 | `constant_value`: TRUE = 0 / FALSE = 1 / NULL = 2 |
| 4 | `is_instance_type` (inline `Type`) |
| 5 | `is_instance_type_id` (id into the `TypeTable`) |
| 6 | `and_argument` (repeated `Expression`) |
| 7 | `or_argument` (repeated `Expression`) |

A boolean formula embeds its FIRST operand inline in the parent `Expression` when it is
primitive; the rest ride field 6/7. The reader handles both forms; the emitter flattens the
whole formula into the repeated field (the plain form), which the reader also accepts.

## 3. The TypeTable indirection (`is_instance_type_id`)

kotlinc does NOT inline the `Type` of an `is T` conclusion — it writes
`Expression.is_instance_type_id` (field 5), an index into the containing message's
`TypeTable`: `Package.type_table` / `Class.type_table` = field 30, `Function.type_table` = 30.
kotlinc appends the table AFTER the functions, so the reader pre-scans for it before the main
decode loop (which decodes contracts inline).

`TypeTable.type` = 1 (repeated `Type`), `TypeTable.first_nullable` = 2: kotlinc stores a
nullable variant of type N at `firstNullable + k`, flagging every entry at
`index >= first_nullable` nullable (`type_table_entry`, `src/jvm/metadata.rs`). A
table-referenced type that decodes nullable gets `Ty::nullable` applied on top of the decoded
base. The reader also accepts the inline form (field 4).

krusty's emitter always writes the INLINE `is_instance_type` (field 4) — simpler, and both
kotlinc's reader and krusty's accept it.

## 4. Adjacent wire notes (same PR family)

- `Function.context_parameter` = **field 13** (repeated `ValueParameter`): kotlinc lowers
  leading context parameters (`context(a: A) fun f()`) to leading JVM value parameters but
  keeps them OUT of `value_parameter` in metadata — a caller fills them implicitly from the
  enclosing context instead of positionally. `context_count` rides
  `LibraryCallable`/`FunctionInfo`/`MetadataCallFacts`; the implicit fill is recorded in
  `TypeInfo::context_args`. Each entry's `ValueParameter.name` (field 2) and its type's
  nullability are read too: the classpath call sig is FULL-arity (context + value), so the
  name/default/lambda-shape/vararg facts all carry a context prefix — names prepended,
  `vararg_index` shifted by `context_count` — and per-call-site code strips the prefix with
  `call_sig_without_context` (the same contract as a source function, whose context params are
  leading `params` entries).
- `Type.type_parameter` = **field 7**: the id is the parameter's index in the function's
  `Function.type_parameter` table (field 4); `Type.type_parameter_name` = 9 carries the name
  for by-name readers. Generic receiver/parameter/return types AND contract `is`-conclusions
  reference these ids, so the emitter threads the same `name → id` map (`tps`) through both.
- Inline fns are emitted as facade statics (kotlinc's `public static synthetic`) with
  `Function.flags` `IS_INLINE` (bit 10) and an erased `JvmMethodSignature` — without the flag a
  downstream module would resolve the inline fn as a plain callable.

## 5. Round-trip validation

Decode is covered by unit tests next to the reader: `src/jvm/metadata.rs` decodes real stdlib
contracts (`isNullOrBlank`, `require`, `requireNotNull`, `run`'s `callsInPlace EXACTLY_ONCE`)
from the provisioned stdlib jar, and `src/contracts.rs` decodes source `contract { … }` blocks
(incl. the labeled receiver `this@f`, `&&` compounds, kindless `callsInPlace`). The
encode→decode round trip is `contract_round_trips_through_metadata` in
`src/metadata/builder.rs` (a `ConditionalReturns` contract through emission and back); the
kinded `callsInPlace` forms round-trip through the same path, the kindless form degrades per
§2.
