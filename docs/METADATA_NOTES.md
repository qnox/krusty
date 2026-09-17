# `@kotlin.Metadata` — reverse-engineering notes (Phase 4b)

Goal: emit `@kotlin.Metadata` so krusty output is consumable as a **Kotlin** library (Java consumers
need only the signatures, already matched in Phase 5a). This is the single largest remaining piece —
effectively a re-implementation of `kotlinx-metadata-jvm`'s writer.

## Reference (`fun f(a: Int): Int = a` in `M.kt` → class `MKt`)

> The `d1` byte capture below was taken from kotlinc **1.9.24** when this format was first
> reverse-engineered; the encoding is version-stable so it still holds. The reference toolchain krusty
> is **validated** against today is pinned by the `kotlin-versions` manifest (currently **2.4.0**) and
> self-provisioned by `just kotlinc` — see `harness/run-diff.sh` / `just conformance`.

Annotation values: `mv=[1,9,0]`, `k=2` (file facade), `xi=48`, `d2=["f","","a"]`.

`d1` (one string), bytes after byte→char-identity decode (26 bytes):
```
00 08 0a 00 0a 02 10 08 0a 00 1a 0e 10 00 1a 02 30 01 32 06 10 02 1a 02 30 01
```

## Encoding chain (implemented + validated, `metadata/encoding.rs`)
`Package` proto bytes → `bytesToStrings` (byte→char identity; **matches the reference d1 exactly**)
→ modified-UTF-8 in the constant pool. `BitEncoding` default path (`FORCE_8TO7=false`); no marker.

## Decoded structure (proto field numbers from `core/metadata/src/metadata.proto`)
- `Package.function` = field 3 (repeated).
- `Function`: `name`=2 (id in d2), `return_type`=3 (`Type`), `value_parameter`=6 (repeated),
  `flags`=9 (default 6 = public final); JVM ext `method_signature`=100 — **omitted** when the JVM
  descriptor is derivable from the Kotlin signature (the simple case).
- `Type.class_name` = field 6 (fq-name id).
- `ValueParameter`: `name`=2, `type`=3.
- `StringTableTypes` (jvm_metadata.proto): `record`=1 (repeated `Record`); `Record.predefined_index`
  = field 2 → index into `PREDEFINED_STRINGS`.

The reference `Function` (`1a 0e …`) decodes cleanly:
`name=0("f")`, `return_type=Type{class_name=1}`, `value_parameter={name=2("a"), type=Type{class_name=1}}`.
The string table has 3 records: `[{}, {predefinedIndex=8}, {}]` — the empty d2 slot index 1 resolves
to the builtin via `predefinedIndex=8` = `kotlin/Int`.

## Builtin `predefinedIndex` table (`JvmNameResolverBase.PREDEFINED_STRINGS`)
```
0 Any  1 Nothing  2 Unit  3 Throwable  4 Number
5 Byte 6 Double 7 Float 8 Int  9 Long 10 Short 11 Boolean 12 Char
13 CharSequence 14 String  …
```
⇒ krusty types: Int→8, Long→9, Double→6, Boolean→11, String→14, Unit→2.

## Leading `00` — RESOLVED
The "extra leading `00`" is the **`UTF8_MODE_MARKER`** (`BitEncoding`): the d1 payload begins with a
`0x00` byte before the delimited `StringTableTypes`. The reader strips it before
`parseDelimitedFrom`. krusty emits it verbatim; confirmed by the round-trips below.

## Class metadata (kind=1) — `ProtoBuf.Class`
Reverse-engineered from kotlinc for `class Point(val x: Int, var y: String)` (see
`metadata/class_builder.rs`). `d1 = 00 <delimited StringTableTypes> <Class>`, k=1, mv=[1,9,0], xi=48.

`Class` fields: `f3 = fq_name` (a string-table class-id), `f6 = supertype` (`Type`),
`f8 = constructor` (repeated), `f10 = property` (repeated). Class flags (f1) omitted ⇒ default
(public/final).
- `Type.class_name = f6`.
- `Constructor`: `f2 = value_parameter` (repeated `{f2=name, f3=Type}`), `f100 = JvmMethodSignature`
  ext (`f2 = desc`; name omitted ⇒ `<init>`).
- A constructor's value parameters carry the SAME `ValueParameter` shape a function's do, and that is
  the only place a constructor parameter's source-level type survives: `Base(init: Cfg.() -> Unit)`
  erases to `Function1` in the `<init>` descriptor AND in its `Signature` attribute, so only the
  `@ExtensionFunctionType` annotation on `ValueParameter.type` (`f3`) says the parameter is a RECEIVER
  function type. A consumer that decodes constructor records for names/defaults only will bind a lambda
  argument with no implicit `this`. krusty therefore decodes `f3` here as well as in `Function`.
- `Property`: `f2 = name`, `f3 = return_type` (`Type`), `f11 = flags` (emitted as **1798** only for a
  `var`; `val` ⇒ 0, omitted), `f100 = JvmPropertySignature` `{f1 = field (empty ⇒ derived backing
  field), f3 = getter JvmMethodSignature, f4 = setter (var only)}`.
- The accessors named by `JvmPropertySignature` are a REALIZATION, not a declaration. Kotlin has no
  accessors: `Dispatchers.IO` is a property, and `getIO` exists only in the class file. So a consumer must
  not make resolving a property read depend on finding a method — `@JvmStatic` on a property of an
  `object`/companion leaves the metadata shape identical (still an ordinary member `Property`) while
  kotlinc emits `getX`/`setX` as JVM **statics** of that class, which are not instance members and never
  will be found that way. krusty reads this table only in the backend
  (`Classpath::property_read_access`), where it also picks up a `@JvmName` or value-class-mangled
  spelling and the `ACC_STATIC` bit that says the accessor takes no receiver.
- A property's return `Type` may instead be referenced by `return_type_id` (`f9`). Its nullable flag
  lives on that metadata `Type`, not in the getter's JVM descriptor or generic `Signature`; classpath
  decoding therefore retains it separately when specializing a generic extension-property result.
- `JvmMethodSignature`: `f1 = name`, `f2 = desc`.
- Applied annotations (kotlinc 2.4.10, `class C { @Mark fun f(): Int = 1 }`): `Function.annotation`
  = f12, `Constructor.annotation` = f3, `Class.annotation` = f25, each an `Annotation` message
  `{f1 = id (a DESC_TO_CLASS_ID string-table entry), f2 = argument {f1 = name, f2 = Value}}`. The
  declaration's flags word gains bit 0 (`hasAnnotations`): a plain member goes 6 → 7, a secondary
  constructor 22 → 23. Both non-SOURCE retentions (RUNTIME and BINARY) go in this one list, even
  though the class file splits them across `RuntimeVisibleAnnotations`/`RuntimeInvisibleAnnotations`.
  INTERNING order ≠ serialization order: the `JvmMethodSignature`/`JvmConstructorSignature` extension
  (f100) interns its `d2` strings BEFORE the annotations, while the annotation field serializes first
  (ascending field number). An annotated `suspend fun f(a: Int)` therefore lands
  `…, "a", "(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;", "Lp/Mark;"` in `d2`.
- The PRIMARY constructor's flags word is the one case where `| HAS_ANNOTATIONS` is not enough.
  `Constructor.flags` has proto default 6 (visibility PUBLIC) and is omitted at that value, so krusty
  carries a public primary ctor as 0; the bit forces the field out, and the default has to be
  materialized on the way (0 → 6 → 7). `class C @Mark constructor(val x: Int)` in kotlinc 2.4.10:
  `08 07 | 12 06 …value parameter… | 1a 02 08 06 (Constructor.annotation, d2[6] = "Lp/Mark;") |
  a2 06 04 …constructorSignature…`, with `Lp/Mark;` interned between `"(I)V"` and `"getX"`. A
  secondary ctor never hits this — its 22 is already explicit, so 22 → 23 is a plain OR.
- A PROPERTY-targeted annotation has no class-file declaration to sit on: kotlinc emits a synthetic
  `getX$annotations()V` method (`ACC_PUBLIC|ACC_STATIC|ACC_SYNTHETIC` + a `Deprecated` attribute)
  carrying the annotation attribute, names it from `JvmPropertySignature.syntheticMethod` (f2), and
  records the annotation as `Property.annotation` = f14. A FIELD-targeted one instead goes on the
  backing field, with `Property` f34 as its metadata record. Either sets `hasAnnotations` in
  `Property.flags` (8710 → 8711) — and because kotlinc derives an accessor's DEFAULT flags word from
  the property's (that bit included), an annotated property with plain accessors also writes
  `getter_flags`/`setter_flags` = 6 explicitly.

String table for a class id: `Record.f3 = 2` (operation `DESC_TO_CLASS_ID`) over the descriptor
`Lpkg/Name;`; builtins via `Record.f2 = predefinedIndex`; everything else verbatim. krusty emits one
record per string (no range compression) ⇒ semantically equivalent, not byte-identical, to
kotlinc — accepted by the reader, which is the ABI goal.

## Package property records (k=2) — kotlinc 2.4.0 observed encoding
Decoded from kotlinc output over a top-level property shape matrix (`Package.property` = f4;
`Property`: `name`=2, `return_type`=3, `receiver_type`=5, `setter_value_parameter`=6,
`getter_flags`=7, `setter_flags`=8, `flags`=11, `JvmPropertySignature` ext=100).

| shape | f11 flags | notes |
|---|---|---|
| `val a = "hi"` | 8710 | 518 base + `hasConstant`(1<<13); const-literal initializer only |
| `val b = run { … }` | omitted (=518) | computed initializer ⇒ no `hasConstant`; f11 elided at wire default |
| `const val c = 7` | 10758 | + `isConst`(1<<11); f100 = field entry ONLY (no getter method exists) |
| `val d get() = 5L` | omitted | f7 (getter_flags) = 70 = public·final·`isNotDefault`(1<<6); NO field entry |
| `var e: Double? = null` | 1798 | `isVar`(1<<8)+`hasSetter`(1<<10); field entry records desc `Ljava/lang/Double;` (boxed) |
| `lateinit var f: String` | 5894 | + `isLateinit`(1<<12) |
| `private val p1 = 3` | 8706 | visibility bits (f>>1)&7: INTERNAL=0, PRIVATE=1, PUBLIC=3; NO getter sig |
| `internal val p2` | 8704 | getter sig present, unmangled |
| `var s1 … set(v){…}` | 1798 | f6 = setter value parameter `{name,type}`; f8 = 70 |
| `val String.doubled get()` | omitted | f5 receiver; f7 = 70; NO field entry |
| `val lz by lazy { … }` | 33286 | + `isDelegated`(1<<15); field entry `{name="lz$delegate", desc="Lkotlin/Lazy;"}` |

Flag layout (property word): bit0 hasAnnotations · 1-3 visibility · 4-5 modality · 6-7 kind ·
8 isVar · 9 hasGetter · 10 hasSetter · 11 isConst · 12 isLateinit · 13 hasConstant · 15 isDelegated.
Accessor word: bit0 hasAnnotations · 1-3 visibility · 4-5 modality · 6 isNotDefault.
String interning is SOURCE-DECL order (a property's setter-param name interns before a later
property's name; function names may intern after property strings even though `Package.function`=3
serializes before f4) — relevant only to byte identity, not to consumption.

## Annotation elements beyond `d1`/`d2` — `pn` (declared package)

`@Metadata` carries the DECLARING Kotlin package in `pn` (an `s`-tagged String element) whenever
`@JvmPackageName` moved the emitted class out of that package. It is absent on every unrelocated
class, where the class's own JVM package is the declared one. Reading it is not optional for a
consumer: kotlin-test's JUnit5 variant declares `package kotlin.test` and emits its facade to
`kotlin/test/junit5/annotations/AnnotationsKt`, so `kotlin.test.Test` is only findable through `pn`
(the `.kotlin_module` catalog records the same fact for jars — see `read_kotlin_module`'s
`jvm_package_name` table — but a class directory need not carry one). `classreader` decodes it once
into `KotlinMeta::package`; every consumer keys declarations by that field rather than by splitting
the class's internal name.

**Writer side — OPEN.** krusty cannot yet compile a source carrying `@file:JvmPackageName`, so it
emits no `pn`. Closing it is four pieces: `-Xfriend-paths` (the annotation is `internal` to the
stdlib, so no source can name it otherwise), emitting the facade class at the relocated JVM package,
writing `pn`, and writing the `.kotlin_module` `jvm_package_name` table with
`class_with_jvm_package_name_{short_name,package_id}`. Reachable only by a library that names an
stdlib-internal annotation — kotlin-test itself, essentially.

## Known byte-identity gap — `Type.abbreviatedType`

kotlinc records the ALIAS SPELLING at a use site: a declaration written `fun make(): Cargo` where
`typealias Cargo = Payload` encodes `Type{class_name=Payload, abbreviated_type=Type{…Cargo…}}`.
krusty writes only the expanded target, so any consumer that spells a typealias in a DECLARED type
differs from kotlinc in `d1`/`d2` alone (bytecode, descriptors and the constant pool all match).
This applies to every classpath typealias, relocated facade or not — measured both ways.

This is the byte-identity blocker that real code hits, and it is a `Ty` REPRESENTATION change, not a
writer patch: every metadata entry point (`class_builder::build_class`, `builder`, `type_encoder`) is
driven by `Ty`, which is fully expanded by the time it reaches them and carries no alias identity.
Emitting `abbreviatedType` means threading the source spelling from `TypeRef` through `Ty` to every
declared-type encode site — the same shape as the in-flight `Ty` nullability migration, and owed its
own workstream.

## Status — round-trips PASSING
Encoding chain ✅, schema + builtin table ✅, `UTF8_MODE_MARKER` ✅. **Both round-trips pass**: a
*Kotlin consumer* compiled by the real kotlinc resolves krusty's top-level functions (facade
`@Metadata` + `META-INF/*.kotlin_module`, Phase 5b) **and** uses krusty's classes via property syntax
(class `@Metadata` kind=1, Phase 8b). Remaining: richer language surface (data classes, methods in
bodies, generics, nullability) — each extends these same builders.

## `Type.abbreviatedType` — the source spelling of a declared type

Kotlin records BOTH forms of a declared type: the expanded classifier and, when source spelled a
`typealias`, the spelling itself. With `typealias Cargo = Payload`, `fun make(c: Cargo): Cargo`
writes `Type{class_name=Payload, abbreviated_type=Type{type_alias_name=Cargo}}`.

Field numbers below were read off **kotlinc 2.4.10** output, not recalled:

- `Type.abbreviated_type` = **field 13** (length-delimited `Type`).
- The alias inside it is `Type.type_alias_name` = **field 12** — a varint string-table class id over
  the descriptor `Lpkg/Alias;`, and it appears INSTEAD of `class_name`, never alongside it.
  (Third-party writeups commonly give field 10 for `type_alias_name`; that is wrong for 2.4.x.)
- `abbreviated_type_id` (field 14) is never emitted — kotlinc inlines the message.

Observed rules, each pinned by a fixture in `tests/typealias_abbreviated_type_e2e.rs`:

| source | encoding |
|---|---|
| `fun make(c: Cargo): Cargo` | `{class_name=Payload, abbreviated_type={type_alias_name=Cargo}}` |
| `List<Cargo>` | the abbreviation is on the ARGUMENT's `Type`; `List` carries none — it is PER NODE |
| `Cargo?` | the abbreviated `Type` REPEATS `nullable` (f3) |
| `Chain` (`typealias Chain = Cargo`) | only the OUTERMOST alias is recorded — `Chain`, never `Cargo` |
| `Boxed<Int>` (`= PBox<T,T>`) | expanded takes 2 arguments, abbreviated takes the 1 source wrote |
| `Handler<Int>` (`= (T) -> String`) | same over a function-type expansion (`kotlin/Function1`) |
| `CargoBox` (`= PBox<Cargo, Cargo>`) | the RHS spelling propagates: BOTH expanded arguments abbreviate to `Cargo` |
| `Boxed<Cargo>` (`= PBox<T, T>`) | the USE SITE's spelling reaches the expansion through the alias's parameters — both expanded arguments abbreviate to `Cargo` |
| `List<out Cargo>` | the abbreviation sits on the type INSIDE the projection wrapper |
| `import dep.Payload as P` | an import RENAME is not a typealias — NO abbreviation |
| `(Cargo) -> Cargo` | an INLINE function type abbreviates its components; the arrow node itself spells nothing |
| `vararg xs: Cargo` | spelled as the ELEMENT, recorded as `Array<Cargo>` — the abbreviation goes on the element and on `vararg_element_type`, never on the array |
| supertype, type-parameter bound, property type, extension receiver, ctor param, member fn | all carry it |

**Interning order** (what keeps `d2` byte-identical): at every `Type` node — the main one and the
abbreviated one alike — the classifier reference interns FIRST, then its arguments. The abbreviated
node interns after the main node's classifier and arguments. Package-member strings intern in
SOURCE DECLARATION order across kinds, `Package.type_alias` (f5) included, even though f5 is
written last.

`TypeAlias` itself distinguishes the two forms in the same way:

- `underlying_type` (f4) is the right-hand side **as written** — every node that named an alias is a
  bare `type_alias_name` reference, recursively, and nothing is abbreviated.
- `expanded_type` (f6) is that side fully expanded, WITH abbreviations.

The two forms differ in exactly one further place, and it is easy to get backwards: an
`abbreviated_type`'s OWN arguments are EXPANDED, each carrying its own abbreviation.
`fun nested(x: Boxed<Cargo>)` writes
`abbreviated_type = {argument={class_name=Payload, abbreviated_type={type_alias_name=Cargo}}, type_alias_name=Boxed}`
— not a bare `Cargo` reference. `underlying_type` is the only place the spelling goes all the way
down.

So `typealias Chain = Cargo` writes `f4 = {type_alias_name=Cargo}` (no `class_name` at all) and
`f6 = {class_name=Payload, abbreviated_type={type_alias_name=Cargo}}`.

### Implementation

`Ty` is fully expanded and cannot carry the spelling, and it is `Copy + Eq + Hash` and interned, so
an alias slot on it would split every structural type comparison on pure surface syntax (kotlinc
keeps abbreviation off type equality for the same reason). The spelling therefore travels beside it:

- `crate::spelling::Spelled` mirrors the `Ty` tree node for node; `DeclaredSpellings` groups one
  declaration's (return, params, receiver, type-parameter bounds, supertypes).
- `File::alias_spellings` preserves the pre-expansion node, keyed by the rewritten node's span,
  because the PARSE SEAM (`expand_fun_type_aliases`) rewrites a same-file alias reference into its
  target and would otherwise destroy the spelling before resolution ever sees it. It is a side
  table rather than a `TypeRef` field because `TypeRef` is embedded by value in the expression
  arena, where the extra eight bytes tripped `Expr`'s size guard — the same reason the spelling is
  not on `Ty`. A reference to an alias declared elsewhere is never rewritten and keeps its spelling
  in `TypeRef::name`, so it needs no entry.
- `resolve::spelling_of_ref` walks a `TypeRef` in parallel with `ty_of_ref_with`, and
  `collect_declared_spellings` records the results on `SymbolTable::declared_spellings` /
  `member_spellings`.
- Class metadata is built from the IR alone, so the spelling reaches it on IR side tables
  (`fn_declared_spellings`, `class_declared_spellings`, `prop_declared_spellings`), filled at
  lowering — the same mechanism `fn_param_declared_nullable` already used.

### Reading it back

A dependency's aliases carry their own spellings, and a consumer inherits them: `parse_type_alias`
(`src/jvm/metadata.rs`) reads `Type.abbreviated_type` off the alias's `expanded_type` and carries the
per-argument spellings through `MetaTypeAlias` -> `Classpath::type_alias_expansion` ->
`AliasExpansion::expansion_spelling`, which `resolve::spelling_of_ref` consults when the alias is not
one of the compiled module's own. So `typealias CargoBox = PBox<Cargo, Cargo>` abbreviates both
expanded arguments whether it is declared in this module or on the classpath.

Only the ARGUMENT spellings are recovered. A node's own abbreviation always belongs to the use site,
which names the alias itself.

## `JvmMethodSignature.desc` — when the descriptor must be recorded

`Function.methodSignature` (extension f100) carries `{f1 = name, f2 = desc}`, both independently
optional. kotlinc omits `desc` whenever a reader can DERIVE the JVM descriptor from the recorded
Kotlin types, and records it when it cannot. Derivation maps each `Type`'s CLASS NAME through a flat
table (`kotlin/Int` -> `I`, `kotlin/collections/List` -> `Ljava/util/List;`, `kotlin/IntArray` ->
`[I`). `kotlin/Array` has no entry there: its descriptor depends on the type ARGUMENT
(`Array<String>` -> `[Ljava/lang/String;`), which a name-keyed table cannot express.

So an `Array` in ANY signature position — value parameter, extension receiver, or return — forces
`desc`. Measured against kotlinc 2.4.10 (`fun` in a file facade; class members and constructors
behave the same):

| declaration | `desc` recorded |
| --- | --- |
| `fun take(xs: Array<Payload>): Int` | `([Lapp/Payload;)I` |
| `fun make(n: Int): Array<String>` | `(I)[Ljava/lang/String;` |
| `fun Array<String>.count2(): Int` | `([Ljava/lang/String;)I` |
| `fun maybe(xs: Array<String>?): Int` | `([Ljava/lang/String;)I` |
| `fun many(vararg xs: Payload): Int` | `([Lapp/Payload;)I` |
| `fun sum(vararg xs: Int): Int` | none (`IntArray` maps on its own) |
| `fun ints(xs: IntArray): Int` | none |
| `fun nested(xs: List<Array<String>>): Int` | none (the array erases away) |

The rule is the ARRAY, not the `vararg`: a `vararg` of a reference element needs `desc` only because
it is recorded as an `Array`. `metadata::descriptor_needs_recording` is the single predicate; the
suspend CPS shape and a signature mentioning a type parameter force `desc` for their own reasons.
Covered by `tests/metadata_array_signature_e2e.rs` (byte-identity vs kotlinc).

### A `vararg` records `Array<out E>`

The recorded `ValueParameter.type` of a `vararg` is `Array<out E>`, not the invariant `Array<E>` the
checker carries — the array's `Argument.projection` is `1` (OUT). The element travels separately and
UNPROJECTED as `ValueParameter.vararg_element_type` (f4). `metadata::vararg_recorded_type` applies
the projection at the encode seam only, so nothing upstream of metadata sees a type source never
wrote. A primitive specialized array has no type argument and is recorded unchanged.

## Class supertypes: only what source declared

`Class.supertype` (f6) lists the DECLARED supertypes, superclass first. An undeclared `kotlin/Any`
is never recorded — `class Holder<T>(val t: T) : Iface` writes one supertype record, not two.

This is a trap for a generic class: its supertype list comes from the recorded generic signature
(`ir.class_signature(..).supers`), which ALWAYS materializes the superclass position because a JVM
class `Signature` attribute must name a superclass, holding `kotlin/Any` when none was declared. The
metadata emitter drops that implicit entry (`jvm/ir_emit.rs`), leaving both the generic and the
non-generic shape with a superclass slot exactly when one was declared.

`DeclaredSpellings::supertype_spellings(has_declared_superclass)` aligns the typealias spellings to
that list. The flag cannot be inferred from the spellings themselves: a declared superclass that
named no alias spells nothing, yet still occupies the slot, so only the emitter knows. Getting it
wrong shifts every abbreviation onto the neighbouring supertype.

## KLIB declaration surface — field numbers read off artifacts

The `.knm` fragments in a KLIB carry the same `PackageFragment` protobuf as `@Metadata` and
`.kotlin_builtins`, so these numbers are not klib-specific — but a klib is where they were
*measured*, by having the reference compiler write a library for a fixture and decoding the bytes.
Every number below came from such an artifact rather than from memory. Two of them contradicted a
comment already in the reader, which is the reason this section exists.

`Package`: `f3 = function`, `f4 = property`, `f5 = typeAlias`, `f30 = typeTable`.

`Class`: `f1 = flags`, `f2 = supertype_id` (packed), `f3 = fq_name`, `f4 = companionObjectName`,
`f5 = typeParameter`, `f7 = nestedClassName` (packed string indices), `f8 = constructor`,
`f9 = function`, `f10 = property`, `f13 = enumEntry`, `f16 = sealedSubclassFqName` (packed
qualified-name ids), `f17 = inlineClassUnderlyingPropertyName`, `f30 = typeTable`, `f175 = file`.

`Function`: `f2 = name`, `f4 = typeParameter`, `f5 = receiver_type`, `f6 = value_parameter`,
`f7 = return_type_id`, `f8 = receiver_type_id`, `f9 = flags`, `f172 = file`. A member extension
(`class Holder { fun Int.f() }`) is exactly the one that carries `f8`.

`Property`: `f1 = old_flags`, `f2 = name`, `f3 = return_type`, `f5 = receiver_type`,
`f6 = setter_value_parameter`, `f7 = getter_flags`, `f8 = setter_flags`, `f9 = return_type_id`,
`f10 = receiver_type_id`, `f11 = flags`, `f173 = compileTimeValue`, `f176 = file`.

`f8` is present only when the declaration states a setter of its own: `var guarded: Int = 1;
private set` writes 66 (private in bits 1-3 plus the `isNotDefault` bit 6) and also writes `f6`,
while a plain `var` omits both and its setter is as visible as the property.

**`Property` f7 is `getter_flags`, NOT a receiver type id.** A comment in the reader said otherwise;
following it would have decoded a flag word as a type. `val String.memberExt: Int get() = 1` records
`f7 = 70`, which is the declared-accessor flag word (public · final · `isNotDefault`), and its
receiver in `f10`. Note also that a `Property`'s return type id is `f9` while a `Function`'s is `f7`
— the two messages disagree, and reading one's number into the other is the mistake to avoid.

`TypeAlias`: `f2 = name`, `f3 = typeParameter`, `f5 = underlyingTypeId`, `f7 = expandedTypeId`. The
two type ids are distinct facts: `typealias Plain = PBox<Int, String>` records one id in both, while
`typealias Chain = Boxed<Int>` records the spelled `Boxed<Int>` in `f5` and the expanded
`PBox<Int, Int>` in `f7`.

`EnumEntry`: `f1 = name` (string index).

Flag words, measured rather than assumed:

| declaration | word | value | meaning |
| --- | --- | --- | --- |
| `val topVal: Int = 1` | Property f11 | 8710 | `HAS_CONSTANT` |
| `var topVar: String` | Property f11 | 1798 | `IS_VAR` + `HAS_SETTER` |
| `const val topConst: Int = 7` | Property f11 | 10758 | + `IS_CONST` |
| `val Int.topExt get() = …` | Property f11 | *absent* | protobuf default 518 — a plain `val` |
| `value class Meters` | Class f1 | 8198 | bit 13 `IS_VALUE` |
| `fun interface Handler` | Class f1 | 16486 | bit 14 `IS_FUN` |
| `data class Pair2` | Class f1 | 1030 | bit 10 `IS_DATA` |
| `inner class Inner` | Class f1 | 518 | bit 9 `IS_INNER` |

Visibility is bits 1-3 in all three flag words (`Class`, `Function`, `Property`), which is why one
decoder serves them: `1`/`4` → private (`PRIVATE_TO_THIS` is a stricter private), `2` → protected,
`3` → public, anything else → internal.

A value class records only the underlying property's NAME (`Class.f17`); no shipped klib carries
`inlineClassUnderlyingTypeId` (f18). The underlying type therefore comes from that property's own
declaration in `Class.f10`.

An `inner` class records the `IS_INNER` bit and nothing else — no field names its enclosing class,
because that is the nested identity's own owner (`Outer.Inner` → `Outer`). A plain nested class is
recorded identically minus the bit.

Annotations travel in `KlibMetadataProtoBuf`'s extension field **170**, on a `Class`, a `Function`
and a `Property` alike; the carrier this reader writes uses `f12` for the same repeated field, and a
declaration carries one or the other, never both. `Annotation` is `{ id = 1 (a qualified-name id),
argument = 2 }`, and `Argument` is `{ name_id = 1, value = 2 }`.

`Argument.Value` carries its kind in `f1` and its payload in the field that kind selects. Read off a
klib written for an annotation with one element of every kind:

| kind | ordinal | payload |
| --- | --- | --- |
| `BYTE` `CHAR` `SHORT` `INT` `LONG` `BOOLEAN` | 0 1 2 3 4 7 | `f2`, **zigzag** (`'x'` writes 240 for 120; `-3` writes 5) |
| `FLOAT` | 5 | `f3`, fixed32 |
| `DOUBLE` | 6 | `f4`, fixed64 |
| `STRING` | 8 | `f5`, a string index |
| `CLASS` | 9 | `f6`, a qualified-name id |
| `ENUM` | 10 | `f6` (the enum class) + `f7` (the entry name) |
| `ANNOTATION` | 11 | `f8`, a nested `Annotation` |
| `ARRAY` | 12 | `f9`, repeated `Value` |

Only `ENUM` and `ARRAY` are read today, for `@Target`: it records ONE argument whose value is an
`ARRAY` of `ENUM`s, and a single-target application uses the same array with one element. The rest
of the table is recorded so it need not be measured again — the values a consumer would want next
are `@Deprecated`'s message (`STRING`) and its level (`ENUM`), which need a carrier for arguments on
a member record, and `@Retention`, whose `LibraryType` field holds the JVM `RetentionPolicy`
spelling rather than Kotlin's `AnnotationRetention` and so is not a carrier-independent decode's to
fill.

### What the Kotlin/Native stdlib actually contains

Counted over the 486 `.knm` fragments of `klib/common/stdlib`, because it is the library every
Kotlin/Native compilation resolves against and its shape decides which of these facts matter:

- 940 classes: 17 `value class`, 3 `fun interface`, 10 `data class`, 9 `inner`, 26 `expect`.
  Every one of the 26 is an annotation class carrying `kotlin.OptionalExpectation` — the
  `kotlin.jvm.*` set (`JvmStatic`, `JvmName`, `JvmField`, …) and the `kotlin.js.*` set (`JsExport`,
  `JsName`, `JsStatic`, …). So "expect annotation" and "optional expectation" coincide in this
  library, but the annotation is what says so and is what should be read: an ordinary `expect`
  without an `actual` is an error, not an erasure.
- 37 top-level type aliases, including `kotlin.collections.LinkedHashMap` → `HashMap` and
  `kotlin.collections.LinkedHashSet` → `HashSet`. On this target they are aliases, not classes.
- Class visibility: 553 public, 295 internal, 92 private.
- Member functions: 2520 public, 293 private, 67 protected, 84 internal. A reader that does not
  decode member visibility reports every one of those 444 non-public members as public.
- 952 of 2964 member functions are annotated (1449 annotations, 607 of them with arguments), and
  103 of 1603 member properties. The most common are `kotlin.internal.IntrinsicConstEvaluation`
  (521), `kotlin.internal.InlineOnly` (223), `kotlin.native.internal.TypedIntrinsic` (127),
  `kotlin.IgnorableReturnValue` (121), `kotlin.Deprecated` (90) and `kotlin.SinceKotlin` (83).
- No identity is declared twice, so a reader that keeps the first declaration of a name is not
  making an order-dependent choice — for this library.
