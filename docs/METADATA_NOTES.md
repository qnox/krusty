# `@kotlin.Metadata` — reverse-engineering notes (Phase 4b)

## Declared class receivers and checked member applications

Source KLIB method exports retain the complete declared self application recorded during FIR
lowering, in the checker's own-and-captured parameter order, including captured-only inner classes.
The exported receiver and parameters keep declaration-owned formals. A dependent checked call
already carries its applied parameter/result types, including applications selected through an
inherited receiver. The common dependency-signature boundary consumes those types for
class-dependent slots while authenticating fixed types and callable type-argument records.
This preserves one declared ABI for distinct Int, String, Long and nullable applications.

## Classpath annotation query boundary

`declaration_annotations` borrows the existing classpath for exact selected-method and all-method
annotation queries. `class_entry_bytes` retains first-entry precedence and the existing directory
`this_class` check, with the same archive, jimage and ct.sym reads. Neither collaborator creates a
cache or changes metadata authority. Parsed class/field annotations stay with `ClassInfo`/`FieldSig`;
classreader decodes arguments and JVM providers interpret them. No annotation interpretation or
new lookup enters core or lowering.

## Annotation classifier kind and JVM dispatch

The JVM annotation realizer retains the exact declared interface in the generated carrier's
`annotation_impl_of` identity. A fresh allocation exposes that representation to its consumer;
reading a declared annotation parameter keeps the parameter's own slot contract. Existing selected
Any-method roles and that construction evidence own interface dispatch and receiver materialization,
without member spelling, generated-name checks, metadata changes or common semantic selection.

Synthetic annotation method headers reserve their existing enabled nullability descriptors before
body constants. Constructor parameter and equals annotations retain their payloads; shared toString
finalization applies the same return contract to memberless implementations. The existing release
policy and class-writer annotation filter own both reservation and attachment.

## Native bound-receiver physical storage

The common callable-reference record owns the bound-receiver capture coordinate. Native emission
uses that coordinate to choose semantic object storage, even when a primitive or value class has
an unboxed invocation carrier. Stored types and offsets jointly own the GC map and thunk loads.
This representation choice changes no declaration identity, metadata or source-level type, and
does not reconstruct receiver roles from a capture's spelling or machine carrier.

## Null-pointer exception storage

The platform NullPointerException declaration carries the existing runtime-exception storage role.
Its metadata owns its superclass; a same-named foreign declaration or one with additional state
receives no role. Wasm reference assertions use the authenticated hierarchy and base Throwable
message/cause fields rather than manufacturing an untyped exception object.

## Native reference identities and exported entry points

A KLIB export's physical invocation alias is independent of its declaration marker. The Native
export handoff carries the source key so a dependent module agrees with references created in the
declaring source module. Dependency-support keys encode the exact module and Public/Accessor
signature through the existing declaration-identity encoder, retaining every segment, member ID and
mask. This changes target symbol publication without changing Kotlin metadata or runtime layout.

## Declaration-owned Class header IDs

The common Class schema takes a mandatory resolved class ID. Its package and exact lexical
segments determine JVM `pkg/Outer.Inner` spelling, predefined records and descriptor compression;
compression is unavailable when a retained segment contains a literal dollar. KLIB writes those
same segments into its qualified-name table and reuses the Class header ID for field 174's class
inventory. Its root/child declaration traversal uses retained ownership too, so literal-dollar
classes cannot disappear before serialization. Explicit local JVM names retain the existing
local-name record and enum-body handling. `companionObjectName` and the companion's `nestedClassName`
retain the same exact owner-relative segment, including a dollar inside a named companion.
Unknown declaration facts never activate a spelling-derived fallback. Other type/class-ID fields
remain separate consumer migrations.

## Dependency conversion proof

Ordinary body conversions and prepared-inline templates consume the same explicit semantic class
model with the dependency declaration's complete formal bounds layered over it. Known empty bounds
remain declaration evidence; they do not fall through to another provider. No class model means
identity-only ordinary proof. Value/reference conversions retain typed common IR operations rather
than changing metadata or runtime layouts. Tests: `klib_lowering::type_evidence::tests` and
`klib_lowering::tests::body_forms`.

## Native source callable-reference identity

The checked source-module callable ID and source-module identity own the Native reflection marker's
linkage. Weak definitions in dedicated COMDAT sections give all emitted files the same address,
which the existing reference equality and hash runtime reads. The invocation adapter and carrier
remain separate from declaration identity. KLIB references need their retained module-qualified
signature before receiving the same program scope; a raw provider-local callable number is
insufficient across independently analyzed modules.

## Identity-keyed declaration-name projection

Providers project name evidence directly from their selected classifier and alias records. Typed
ordinary/alias keys retain both declarations when their flattened identity is the same. The
outer option distinguishes an absent declaration from a selected declaration whose name is unknown.
Federation stops at the first present record; source-unit caches retain all three states. Frozen
backend module facts keep source class and alias names independently of alias targets, and forward
only absent identities to the dependency inventory. Literal-dollar ownership stays snapshot-owned.
The metadata encoder callback and class-ID sinks remain a separate consumer change.

## Alias declaration-name evidence

Package and Class type-alias tables supply the alias's exact simple name and declaring container.
Package metadata honors its `pn` relocation; classifier ownership comes from the raw Class
`fq_name` evidence, independently of the alias's existing lookup key and physical classfile owner.
An unavailable classifier declaration name stays unknown. Classpath indexes, paired stdlib lookup
and provider normalization retain that fact independently of target normalization. Metadata output
still needs the separate identity-keyed declaration-name lookup; alias semantic keys are not
descriptor payloads and an expansion target's owner never substitutes for the alias's own owner.

## Source inventory publication

Compact source headers publish classifier identities from explicit declaration ownership and exact
segments rather than splitting lookup names on dots and dollars. This avoids creating undeclared
prefix identities before later consumers run. Metadata class-ID encoding still needs its separate
snapshot ownership lookup; publication alone does not supply authoritative negative nesting facts
for every cached or cross-module identity.

## Wasm reference-array representation

Reference-array reads consume the selected callable's frozen ArrayGet operation and the checked
array element type; no classifier/member spelling lookup is added. Physical elements share nullable
reference storage, with ordinary backend coercion restoring the checked result carrier. Bounds
failures consume the platform exception storage evidence already retained beside classifiers.
Array range-check defaults and overrides are Wasm representation policy and change no metadata,
provider identity or frontend overload decision.

## Platform exception storage authority

Only the explicitly selected platform stdlib may publish an admitted runtime-exception storage
role. Class shape, generic/outer-instance state and metadata supertypes constrain that role;
subclasses with additional declared instance state are not admitted. The frozen classifier record
retains the role and exact parent identity. Wasm's physical registry consumes those records rather
than selected constructor inventory or classifier spelling, and validates the complete chain even
when its leaf already has a GC type. Ordinary source and JVM classifier records retain no role.

## Companion fields and class metadata

Physical companion storage uses the whole segment within the recorded holder, never the last
dollar-delimited fragment. This field-ABI change preserves metadata production independently.
The declaration-owned metadata header work retains the separate whole-owner classfile oracle;
field inventories and complete caller classfiles validate this change without omitting any field.

## JVM member-class rows

The JVM attribute writer now consumes retained non-local ownership and exact declaration segments
for member rows, including names with literal dollars. The cross-file module resolver preserves
the current declaration's precedence over classpath rows. Local and anonymous implementation rows
retain their existing provenance. This changes no Kotlin metadata class-ID encoding; aliases and
builtin ownership evidence belong to the remaining metadata handoff.

## Builtin declaration-name evidence

Capture raw builtin Class names before name-tree normalization, so a literal dollar remains
distinct from the metadata's classifier separator. The selected builtin signature carries its
own declaration identity and name; a mapped physical class does not supply that semantic owner.
Exact physical classifiers retain their own classfile/Kotlin metadata authority. Compiler-owned
function, array, reflection and JVM Cloneable factories publish names from their existing
declaration definitions. These facts are input to the later metadata name consumer; this producer
change does not itself repair the retained complete-classfile literal-dollar regression.

## Declaration-name evidence

Kotlin metadata class IDs delimit the package with `/` and lexical classifier ownership with `.`;
`$` within either segment is literal content. Normalization retains that exact declaration segment
and package or classifier parent in the common classifier shape. Source headers and structural
classfile self entries publish the equivalent fact, independently of outer-instance capture.
Resolved source headers, common IR and backend snapshots retain it without consulting global
interned-name nesting. Missing evidence remains unknown. This phase changes no metadata encoding;
the emission consumers will adopt the fact in a separate follow-up.

For JVM dependencies, retain ownership from raw `Class.fq_name`, before flattening package and
classifier delimiters for rendering. `StringTableTypes.local_name` indices exclude local class IDs
independently of visibility, including public members of local classes. Actual resolved metadata
annotation presence chooses Kotlin authority even when the decoded declaration inventory is empty;
the inventory's inferred `is_present()` query cannot establish annotation absence.

JVM generic result consumption joins the retained declaration-result coercion with its physical
producer after erasure. Null assertions preserve that producer's slot, and value-class lowering
uses the same marked edge to distinguish declaration carriers from generic boxes. The extra
nullable-to-nonnull coercion retains that declaration boundary. Source metadata types continue
to describe the selected declaration independently of this JVM representation decision.

## Source and optional-annotation projection

The source index already owns each classifier's optional declaration name; its provider projection
copies that fact without reinterpretation. A `.kotlin_module` optional-annotation class supplies a
raw qualified class name even though no physical classfile exists. Retain its package/owner and
exact segment before name-tree normalization, then publish that fact with the annotation shape.
This preserves literal-dollar and nested names across separate metadata snapshots.

## Closed annotation array operands

Checked spreads and named whole-array operands contribute their elements to one annotation array
payload. Both classfile annotation attributes and Kotlin metadata consume that same flattened value;
ordinary operands remain individual elements. The recorded argument role owns this distinction.

## Serialization constructor handoff

The serialization constructor's `named_params` retain nullable reference elements and the nullable
serialization marker independently of the erased constructor parameter vector. The handoff test
checks both exact vectors; the generated owner is also compared byte-for-byte with the matching
official serialization plugin. This updates test coverage without changing metadata encoding.

## Operand-holder physical reservation

Common IR distinguishes an argument-evaluation holder from an expanded parameter or capture by
semantic purpose, including through expression cloning and formal-receiver adoption. The JVM frame
reserves an argument-order holder before its initializer uses scratch storage. Expanded formal
bindings retain their existing timing. The holder keeps its CallOperand identity, while its semantic binding
is published at the store. Before assignment, frame snapshots see the unpublished reservation as
Top. Debug-local lifetimes still begin after the store; checked types, parameter metadata, argument
evaluation order and the compiler/plugin wire model are unchanged.

## Anonymous superclass argument bindings

Base expressions of an anonymous object are checked before its dispatch-property scope exists.
The selected lexical value and complete applied parent flow through ordinary constructor checking;
an inherited property of the same spelling does not change generic inference. Complete JVM class
headers and member tables are compared with kotlinc in the constructor-alias regression. The original
expanded #2655 program and both ABI/runtime controls live in
`tests/typealias_constructor_inference_e2e/parameterized_parent_aliases.rs`, alongside the separate
minimal controls. No metadata wire fields, declaration identities or backend-specific substitutions
change at this boundary.

## Class formal layouts before member publication

Kotlin class metadata supplies own formals and bounds. A mapped builtin declaration supplies its
semantic layout when applicable; Java class signatures supply the remaining declarations. Captured
class formals come from non-static `InnerClasses` outer edges, with bound hydration before own-first
capture/shadow renaming. Local or anonymous entries without an outer are not member capture edges.

Inferred exposed anonymous results are approximated before their declaration signatures enter
metadata. Their authenticated own/captured application supplies the declared parent and bounds;
ordinary written stars retain their declaration meaning. Function parents keep the complete
semantic function shape. Private factory and lambda implementation signatures remain exact rather
than being rewritten to match the exposed declaration's denotable result.
The retained body view removes solver-local callee parameters through the same lexical formal set;
ordinary result denotability does not introduce a separate checking context.
Parent templates are the same alias-expanded compact facts later published in classifier headers;
metadata and anonymous exposure do not reconstruct arguments from transitional collector shapes.

Package metadata retains the same authenticated local-classifier and enum-entry-body inventories as
class metadata. Function and backing-field signature records use that locality evidence too: a
reader cannot reconstruct a local classifier's physical JVM name from its metadata class ID.
The original #2654 private-factory, explicitly typed getter and inferred-getter controls compare
complete decoded and raw payloads; the inferred-getter and property-storage controls also compare
the complete class ABI. Tests: `tests/facade_local_classifier_metadata_e2e.rs`.
Own versus lexical-capture applications remain tracked by #2653 with its unchanged complete fixture
oracles; ABI publication checks do not establish metadata payload parity.

Layout reads use class headers and builtin declaration tables, not the enclosing `LibraryType` or
its members. Physical mapped Java requests keep invariant formals independently of a Kotlin view's
declared variance. This changes no metadata wire fields or physical descriptors. Missing lexical
formal scopes, including unavailable enclosing-method evidence, do not license placeholder bounds.

Field and property-accessor JVM `Signature` attributes consume the retained semantic formal identity
even when its declaration belongs to an enclosing method. Nullable and bounded formals keep that
identity through the common formatter; class-owned formals and concrete declarations use the same
type-use boundary. Class formal declarations and Kotlin metadata applications retain their own layout.

Wasm's erased-reference binary equality realization uses the already checked operator and the
runtime GC representation. It changes no metadata, callable identity, or provider authorization;
explicit dependency `Any.equals` realization remains a separate authenticated-callable contract.

Goal: emit `@kotlin.Metadata` so krusty output is consumable as a **Kotlin** library (Java consumers
need only the signatures, already matched in Phase 5a). This is the single largest remaining piece —
effectively a re-implementation of `kotlinx-metadata-jvm`'s writer.

## Applied generic argument completeness

The common subtype relation consumes the complete applied supertype supplied by declaration
evidence. An explicitly applied target must have exactly the same argument count before checking
variance or projections; an empty target remains the existing owner-reachability query. This does
not change metadata encoding or recover missing arguments. The unit hierarchy oracle must preserve
its collection supertype arguments rather than supplying raw declarations for applied queries.

## Java raw type arguments

A Java raw occurrence is a complete application of `RawProjection` arguments, one per formal.
kotlinc 2.4.21 writes it (`fun f() = Raw.box()` for
`class Box<T extends CharSequence>`) as a flexible `Type`: the lower face applies each erasure
(`Box<CharSequence!>`, itself a `PlatformType` flexible argument), `flexible_type_capabilities_id`
is `kotlin.jvm.PlatformType`, the upper face is the nullable all-star application of the read-only
face (`Box<*>?`, `List<*>?` for a raw `java.util.List` whose lower face is `MutableList<Any?>`),
and both faces carry the JVM extension `JvmProtoBuf.is_raw` (field 101) set to true. Strings
intern lower, upper, then the capability (`tests/java_raw_type_e2e.rs`).

## Shared-target compiler/plugin model artifacts

The Rust compiler/plugin model tracks checkout identity as a Cargo input, so a shared target cannot
reuse another checkout's incompatible declaration structure. No serialized plugin fields, protocol
versions, classfile metadata or annotation payloads change.

## Checked annotation constructor results

The checked constructor overlay retains stable declaration identities while its merged inventory
normalizes generic signatures from the classifier's formal layout. Semantic lambda parameter
templates remain available before argument checking; physical constructor parameters and serialized
metadata layouts are unchanged.

Class-literal arguments publish their checked classifier identity before constructor selection.
Nested annotation folding consumes that same identity for its class token; it does not rediscover
the literal from its source spelling. This also covers deeper nested classifier arguments in
dependency annotation payloads and preserves ordinary annotation defaults and enum entries.

## KLIB emission root categories

KLIB dependency-root classification retains authenticated intrinsic-support ownership independently
of successful code emission. A declined wrapper does not become an ordinary body; its metadata and
declaration identity remain unchanged, and ordinary roots retain their strict closure contracts.

## Ordinary bottom-array values

Finalized-expression array validity consumes already checked semantic types, including nested
arguments, and changes no declaration or metadata layout. The class-literal feature policy remains
separate; a suppressed bottom-array value retains its complete existing JVM method signature.
JVM signature elements reuse the existing physical descriptor for `Nothing`, preventing a
nonexistent `kotlin/Nothing` class from appearing in an otherwise nongeneric array signature.

## Constructor parameter annotation pool order

Constructor header reservation interns annotation element names and values, not just annotation
descriptors. This changes only JVM constant-pool visit order; checked payloads, retention and Kotlin
metadata remain unchanged. Strict whole-class comparisons cover both source input orders and mixed
runtime/binary retention with nested annotations, arrays, class literals and nullable parameters.

## Constructor-property annotation publication order

Generic annotation owner syntax and its selected alias identity survive to the checking scope.
Element substitution and nested constant folding consume the checked applied owner/constructor
types, never an erased parameter list or a fresh lookup by annotation spelling. This changes no
annotation wire format. JVM generic class/member `Signature` attributes use the same formatter as
ordinary declarations, with the existing physical `KClass`-to-`Class` annotation representation.

The stable property index receives checked applied-annotation payloads only after every source
annotation class has published its target policy. Per-file checking releases its TypeInfo while
retaining just folded values, explicit use sites and the property identity until that partition.
This changes semantic publication order, not annotation encoding; whole-class oracle checks retain
the same Kotlin metadata and physical annotation attributes in both source input orders.

## Declared JVM vararg array types

The generic signature reader decodes each `ValueParameter.type`/`type_id` independently of its
vararg element field. The former carries the packed array and its projection; the latter supplies
the vararg marker/element call facts. Reconstructing `Array<T>` from that element would discard
`Array<out T>`, and an absent declared type must not be repaired from the element. Inline and
type-table forms share the same decoding path; no metadata encoding changes. Consumers retain the
packed array projection but use the readable element value type for listed-argument conversions,
adapted references and evaluation-order spills; whole-array spreads keep the array type.

Local calls retain the checker-applied value-parameter list for packing too. It may select a concrete
reference array while the lifted local declaration still has an erased generic array descriptor.
This changes neither that declaration's generic signature nor metadata encoding, and does not
reconstruct selected element types from operands or physical descriptors.

## Nested JVM signature argument layout

Common classifier applications store own arguments before captured enclosing arguments. JVM
generic signatures render enclosing segments first, with each segment reading its own semantic
argument range. Declaration variance and raw/Nothing decisions use the same own-first layout.
Kotlin metadata's existing inner-type split and the physical JVM descriptors remain unchanged.

## Reference (`fun f(a: Int): Int = a` in `M.kt` → class `MKt`)

> The `d1` byte capture below was taken from kotlinc **1.9.24** when this format was first
> reverse-engineered; the encoding is version-stable so it still holds. The reference toolchain krusty
> is **validated** against today is pinned by the `kotlin-versions` manifest (its newest release) and
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

## `d1` parts — kotlinc's partition
A payload larger than one `CONSTANT_Utf8` entry is written as several `d1` strings. kotlinc's
`UtfEncodingKt.bytesToStrings` appends each char (the marker included), adds its modified-UTF-8
width (1 for `0x01..=0x7f`, 2 for `0x00` and `0x80..=0xff`), and closes the part as soon as the
count reaches **65534**, so a part holds 65534 bytes, or 65535 when a two-byte char crosses the
threshold. No trailing empty part is written. `metadata::encoding::bytes_to_strings` follows the
same rule; `tests/large_metadata_parts_e2e.rs` compares a 4000-property class byte for byte.

## Version stamp — `mv` and the `.kotlin_module` header

Every emitted `@Metadata` carries `mv` (metadata version) and the `.kotlin_module` header carries
the same version as its first ints after the format marker. The public `-language-version` flag
selects language semantics and, without an override, the `[X, Y, 0]` stamp for that language level.
With no public version option, both semantics and stamp use the selected kotlinc release's stable
default. The internal `-Xmetadata-version X.Y` can override only the artifact stamp on the bounded
2.0–2.4 contract; it never changes source semantics or API availability. One value threads from `crates/krusty-cli`
through `JvmBackend::with_metadata_version` into `EmitOptions::metadata_version`; regenerated
anonymous-object metadata at inline call sites stamps the caller's version, exactly as kotlinc's
`AnonymousObjectTransformer` does. Pinned by
`tests/metadata_language_version_e2e.rs`, which diffs class, facade, `$DefaultImpls`,
suspend-lambda, and regenerated-object `mv` values, plus the 20-byte module header, against kotlinc
run with `-language-version 2.2`.

## Interface compatibility holder table order

An interface's generated compatibility holder uses the shared `InnerClasses` retained-row order
for constant-pool publication. Its nesting relation and Kotlin metadata remain unchanged. Removing
the earlier explicit class seed lets annotation-only references and declared children precede it
when the ordinary table order requires that position.

Equal qualified-name source rows use their retained declaration visitation order within the
declaring writer's slots. Common lowering may evaluate a nested constructor operand first;
that does not change the table's declaration order. Final pool relayout preserves this source
order while retaining pool movement for generated and referenced rows and ancestor constraints.

## Module file — part ordering

Within one package, the `.kotlin_module` part list is sorted by part name
(kotlinc's `ModuleMapping.writePartsWithinPackage` sorts the internal names), not written in
compilation order; packages themselves are ordered by name. Pinned byte-exact by
`package_parts_are_sorted` in `src/metadata/module.rs`. Multifile facades and
`@JvmPackageName` parts-outside-package are not modeled yet.

## Class metadata (kind=1) — `ProtoBuf.Class`
Reverse-engineered from kotlinc for `class Point(val x: Int, var y: String)` (see
`metadata/class_builder.rs`). `d1 = 00 <delimited StringTableTypes> <Class>`, k=1, mv=[1,9,0], xi=48.

`Class` fields: `f3 = fq_name` (a string-table class-id), `f6 = supertype` (`Type`),
`f8 = constructor` (repeated), `f10 = property` (repeated). Class flags (f1) omitted ⇒ default
(public/final).
- `fq_name` resolves to the Kotlin class name in metadata spelling: `/` between package segments
  and `.` between classes (`lib/Outer.Nested`). Dotting the `/`s gives the qualified name
  (`lib.Outer.Nested`), which a consumer cannot recover from the internal name because `$` is also
  a legal identifier character. The serialization plugin names a dependency's `@Serializable object`
  by it (`KotlinMeta::class_qualified_name`).
- `Type.class_name = f6`.
- `Constructor`: `f2 = value_parameter` (repeated `{f2=name, f3=Type}`), `f100 = JvmMethodSignature`
  ext (`f2 = desc`; name omitted ⇒ `<init>`).
- A constructor's value parameters carry the SAME `ValueParameter` shape a function's do, and that is
  the only place a constructor parameter's source-level type survives: `Base(init: Cfg.() -> Unit)`
  erases to `Function1` in the `<init>` descriptor AND in its `Signature` attribute, so only the
  `@ExtensionFunctionType` annotation on `ValueParameter.type` (`f3`) says the parameter is a RECEIVER
  function type. A consumer that decodes constructor records for names/defaults only will bind a lambda
  argument with no implicit `this`. krusty therefore decodes `f3` here as well as in `Function`.
  A context function type (`context(A, B) () -> R`) is the same `FunctionN` with
  `@ContextFunctionTypeParams(count = N)` on the type (`Annotation` field 100, argument `count` as
  a zigzag int). An anonymous context parameter's metadata name is `<unused var>`, not the source
  `_`. Test: `tests/captured_receiver_names_e2e.rs`.
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
- The PRIMARY constructor's flags word is written whenever it differs from the proto default 6
  (visibility PUBLIC). krusty carries the real word (6 for a public primary ctor) and omits it only
  at 6, so the bit takes a public ctor to an explicit 7, and an `internal constructor`, whose
  visibility bits are 0, is written as an explicit 0 (1 with annotations), as kotlinc writes it.
  `class C @Mark constructor(val x: Int)` in kotlinc 2.4.10:
  `08 07 | 12 06 …value parameter… | 1a 02 08 06 (Constructor.annotation, d2[6] = "Lp/Mark;") |
  a2 06 04 …constructorSignature…`, with `Lp/Mark;` interned between `"(I)V"` and `"getX"`. A
  secondary ctor never hits this — its 22 is already explicit, so 22 → 23 is a plain OR.
- The annotation RECORDS are controlled by `LanguageFeature.AnnotationsInMetadata`, stable since
  language version 2.4. With an older source-language level (for example `-language-version 2.2`),
  kotlinc 2.4.20 writes no `Class`/`Constructor`/`Property`/`Function`/`ValueParameter`/`EnumEntry`
  annotation record and interns no annotation string, but still sets every `hasAnnotations` flag
  bit and still records a property's `JvmPropertySignature.syntheticMethod` (the
  `getX$annotations()V` marker exists in the class file regardless). krusty gates only the record
  appends on the finalized feature setting (`ClassTail::annotations_in_metadata`, plus the facade
  builder's matching parameter); flags and the marker pointer are computed exactly as at 2.4. The
  internal metadata-stamp override is independent and cannot enable or disable this source feature.
  Test: `tests/metadata_language_version_e2e.rs`.
- A PROPERTY-targeted annotation has no class-file declaration to sit on: kotlinc emits a synthetic
  `getX$annotations()V` method (`ACC_PUBLIC|ACC_STATIC|ACC_SYNTHETIC` + a `Deprecated` attribute)
  carrying the annotation attribute, names it from `JvmPropertySignature.syntheticMethod` (f2), and
  records the annotation as `Property.annotation` = f14. A FIELD-targeted one instead goes on the
  backing field, with `Property` f34 as its metadata record. Either sets `hasAnnotations` in
  `Property.flags` (8710 → 8711) — and because kotlinc derives an accessor's DEFAULT flags word from
  the property's (that bit included), an annotated property with plain accessors also writes
  `getter_flags`/`setter_flags` = 6 explicitly.
- Constructor-property defaulting is a frontend selection, independent of metadata format gates.
  The configured param-property mode can retain one checked payload at both constructor parameter
  and property (or an available field when the policy does not admit PROPERTY). Explicit use-site
  applications remain single-site and older first-only mode remains distinct. Common metadata
  consumes recorded site membership; stable plugin-visible property publication uses the same
  selection operation over final annotation target policies.
  The KLIB class-surface fixture retains the same property annotation in current language mode.
  A live official Native oracle compiles that unchanged fixture in both older first-only and
  current param-property modes, decodes the complete annotation lists and checks a plain-property
  positive control and an unannotated-property negative control. The full surface assertion is
  preserved; output stamps do not select frontend semantics.
- An annotation written on an accessor (`@A get`, `@A set`) sets `hasAnnotations` (bit 0) in that
  accessor's flags word (`getter_flags` f7, `setter_flags` f8), which therefore differs from the
  default word and is written. Its records go to `Property.getter_annotation` (f15) and
  `setter_annotation` (f16); an annotation on the setter's parameter (`set(@A v)`) sets the bit on
  `setter_value_parameter` and records it in that `ValueParameter.annotation` (f7). Arguments are
  recorded as for any declaration annotation, and only where annotations are written into metadata
  at all. Test: `tests/property_accessor_annotation_e2e.rs`.
- A member property's modality (bits 4-5) follows its declaration, not its class (kotlinc 2.4.20):
  `open`, and an `override` not marked `final`, are OPEN even in a final class; `abstract`, and an
  interface property without a getter, are ABSTRACT; an interface property with a getter is OPEN.
  `lateinit` is bit 12 and a delegated property sets bit 15. The accessor words share the property's
  visibility and modality. A getter source declares, a setter body, a `private set`, and a delegated
  property's accessors set `isNotDefault` (bit 6); a bodiless `set`, annotated or not, stays the
  default. `private set` also narrows the setter's visibility (`setter_flags` = 66). A word is
  written only when it differs from the default one derived from the property:
  `override val p get() = 2` records `getter_flags` = 86 and `flags` = 534.
- A non-default setter records `Property.setter_value_parameter` (f6), and kotlinc serializes it
  before the property's name, so its name and type strings come first in `d2`. An unnamed parameter
  is `value` on a bodiless `private set` and `<set-?>` on a delegated `var` (with or without
  `private set`). An ordinary delegated property's `JvmPropertySignature.field` names its
  `x$delegate` field (desc `Lapp/D;`). A singleton or constant delegate is evaluated in place, so
  that signature omits `field` and records the generated `get<X>$delegate` method in
  `delegateMethod` (f5) instead. Reflection reads the recorded realization. A `var` whose setter
  word says not-default but has no setter parameter fails with `No type in ProtoBuf.ValueParameter`.
  Tests: `tests/in_place_delegate_e2e.rs`, `tests/metadata_property_flags_e2e.rs`, `class_builder` unit test
  `private_setter_records_value_parameter_before_the_property_name`.

- `ReturnValueStatus` lives in `Function.flags` bits 16-17 and `Property.flags` bits 17-18
  (Unspecified 0, MustUse 1, ExplicitlyIgnorable 2). With the return-value checker disabled, the
  default of kotlinc 2.4.0-2.4.20 (`FirMustUseReturnValueStatusComponent`), a declaration takes the
  status of the first declaration it overrides that records one, else Unspecified. The stdlib records
  MustUse on most API (`Any.toString`, `Continuation.resumeWith`, `List.get`) and ExplicitlyIgnorable
  on members such as `MutableCollection.add`; a Java declaration records none and is skipped, so an
  override of `java.util.ArrayList.add` records 2. The same status resolution gives an override the
  `operator` (bit 8) and `infix` (bit 9) modifiers when any declaration it overrides has them. Test:
  `tests/metadata_return_value_status_e2e.rs`. KLIB package functions and properties carry these
  same status bits through target-neutral normalization rather than dropping them at the provider
  boundary.
- Field order and type-parameter references (kotlinc 2.4.0, 2.4.10, 2.4.20): the protoc-generated
  `writeTo` emits every message's fields in ascending field-number order, repeated fields together
  and extensions last, so `Type.flags` (f1, e.g. `SUSPEND_TYPE`) comes first and a function's
  `return_type` (f3) precedes its `type_parameter` (f4). Interning order in `d2` is independent of
  it, yet follows the same order for these two: a function or property, top-level or member,
  interns its return type before its type parameters' names and bounds (`fun <U> f(u: U) =
  Result()` writes `Result`, then `U`). A `Type` naming a type parameter the declaration being written owns (a member function's or
  property's own, bounds included) uses `type_parameter_name` (f9); an enclosing class's uses
  `type_parameter` (f7). A class header is the class's own declaration, so its type parameters'
  bounds and its supertypes name the class's parameters too (`class C<T : Comparable<T>>`,
  `class E<T> : I<E<T>>`), while its constructors and members address them by id. A setter is a declaration of its own, so its value parameter addresses the
  property's type parameter by id (`var <V> Cell<V>.content: V` names `V` in the return and receiver
  types but writes `type_parameter` in `setter_value_parameter`). krusty builds the record in any
  order and `Pb::canonical` sorts it. Test: `tests/metadata_type_reference_e2e.rs`.
- Version requirements (kotlinc 2.4.20, `FirJvmSerializerExtension`): a named, non-private,
  non-suspend inline function with a value parameter (context parameters excluded) or extension
  receiver of a function type (`Function*`, `SuspendFunction*`, `KFunction*`, `KSuspendFunction*`,
  nullable or not) records `version_requirement` (f31) = compiler 1.3.50, unless parameter
  assertions are disabled. The checker decides "function type" from the classifier declaration
  (`LibraryType::represents_some_function_type`) and publishes it per callable; the serializer only
  reads that fact. Property accessors never qualify. The owning `Package` or `Class` keeps
  one `version_requirement_table` (f32); equal requirements share one entry in order of first use,
  members before the class's own (the interface `-jvm-default` 1.4.0 requirement). The entry is
  `{ version (f1) = major | minor << 3 | patch << 7, version_kind (f6) = COMPILER_VERSION (1) }`.
  Test: `tests/metadata_version_requirement_e2e.rs`.
- `JvmMethodSignature` presence (kotlinc 2.4.20, `FirJvmSignatureSerializer.requiresFunctionSignature`):
  a function records its descriptor exactly when the one rebuilt from the declared extension
  receiver, value parameters (context parameters excluded; a vararg as its array type) and return
  type differs from the physical one, or when one of them has no class id (a type parameter). Each
  type maps by its class id, nullability ignored, through `ClassMapperLite` (`kotlin/Int` -> `I`,
  `kotlin/reflect/KFunctionN` -> `Lkotlin/reflect/KFunction;`, anything unlisted -> `L<id with $>;`).
  So `vararg x: Int` records nothing, while a nullable primitive, a `Unit` parameter, a suspend
  function type, `KSuspendFunctionN` or any context parameter does. Test:
  `tests/metadata_version_requirement_e2e.rs`.
- `JvmFieldSignature.desc` presence (kotlinc 2.4.20, `requiresSignature`) follows the same
  mapping: a backing field, a hoisted companion field or a delegate's `x$delegate` records its
  descriptor exactly when it differs from the property type's `ClassMapperLite` descriptor. So a
  type-parameter field (`private var x: T` -> `Ljava/lang/Object;`, with or without accessors), a
  boxed `Int?`, a value class's carrier (`UInt` -> `I`), a reference array (`Array<String>` ->
  `[Ljava/lang/String;`) and a delegate of another type record it, while `IntArray`, `List<T>?` and a
  delegate of the property's own class do not. A property without a field (abstract, or in an
  interface) interns no field string. The descriptor interns after the accessors' strings, so a
  constructor property's comes after its getter's. Test: `tests/metadata_field_signature_e2e.rs`.
- Local classifiers (kotlinc 2.4.20): a local class, an anonymous object, and any class nested in
  one get a full `Class` record with LOCAL visibility (5): members, supertypes (`Any` when none is
  declared) and nested class names. An anonymous object records no constructor. The class id is
  the raw internal name of the classifier declared in executable code, then `.`-separated segments
  for classes nested in it (`app/AKt$make$Local.In`), listed in `StringTableTypes.local_name`.
  Every literal class id (a local one, or a name with `$`) opens a new string-table `Record`, which
  the plain strings after it extend (`JvmStringTable.getQualifiedClassNameIndex`), and reuses an
  equal string only when its locality matches. A member whose parameter, receiver or return type
  names a local classifier at the outermost position records its JVM descriptor, since the
  reader's class-id mapping cannot produce the classifier's JVM name.
  Test: `tests/metadata_local_classes_e2e.rs`.
- Expression-generated classes (kotlinc 2.4.20): a callable-reference class (property, mutable,
  bound, top-level, function, bound-function and adapted references) and an annotation
  instantiation class (`<Facade>$annotationImpl$<Annotation>$N`) carry `@Metadata(mv, k = 3, xi)`
  with no `d1`/`d2`, where `xi = 48 | (LOCAL (5) << 8) = 1328`. Before 2.4.20 `xi` is 48. Not yet
  covered: the public visibility and bit 7 kotlinc writes for such a class regenerated from an
  inline function (`xi = 0x3b0`/`0x5b0`), and the pre-release bit (2) under experimental language
  features. Test: `tests/metadata_synthetic_classes_e2e.rs`.
- A class declared, at any depth, in the body of an `inline` function that is neither private nor
  a member of a private class (an anonymous object, one nested in it, or one in a lambda there)
  is part of that function's ABI: kotlinc writes its `k = 1` record with `xi = 48 | 128 = 176`
  (`METADATA_PUBLIC_ABI_FLAG`). An inlining consumer regenerates only a class carrying the flag.
  Test: `tests/metadata_inline_object_abi_e2e.rs`.
- Member visit order (kotlinc 2.4.20): constructors first, then the class's declarations in source
  order, the primary-constructor properties first and enum entries at their declaration position
  (so after those properties and before the body's other members), then the members the compiler
  generates: `componentN`, `copy`, `equals`, `hashCode`, `toString` for a data class and `equals`,
  `hashCode`, `toString` for a value class. A value class's override that source declares is one of
  its declarations instead: `override fun toString()` is a declared `Function` (realized as
  `toString-impl`) and the generated list keeps only the members source left out. The visit order is the order strings enter `d2` and the
  order within each protobuf list (`function`, `property`, `enum_entry`); the lists themselves stay
  in field-number order. Test: `tests/metadata_member_order_e2e.rs`.
- A boxed value class (`+FullValueClasses`, kotlinc 2.4.20) is still a value class in metadata:
  `IS_VALUE` (class flag bit 13) with no inline underlying property. A final one's generated
  `equals`/`hashCode`/`toString` follow its declarations as SYNTHESIZED records with the data-class
  flags (`0x101d6`, `0x100d6`). An abstract or sealed one has no generated bodies, yet declares each
  `Any` member its source does not override as an open DECLARATION (`0x10116`, `0x10016`) without a
  `JvmMethodSignature` and without a JVM method. Test:
  `tests/metadata_value_class_synthesized_members_e2e.rs`.
  Native's full-value parameter diagnostics do not require a JVM annotation declaration: an
  unresolved bare `@JvmInline` retains its separate annotation-resolution diagnostic. The Native
  oracle tests use the matching stdlib KLIB provider and pin both errors and the independent
  unannotated case without changing metadata.
- Captured type parameters (kotlinc 2.4.20): a local or anonymous class refers to a type parameter
  of an enclosing declaration by id alone (`Type.type_parameter`). Ids come from kotlinc's
  per-declaration interner: the class's own parameters first, then each captured one on first use,
  and a member's first use is numbered in that member's own scope. Test:
  `tests/metadata_captured_type_parameters_e2e.rs`.
- Nested and inner classes (kotlinc 2.4.0, 2.4.10, 2.4.20): a nested class, inner or not, is
  serialized under its outer classes, so every enclosing class's parameters hold the ids before its
  own, outermost first. The class header (type-parameter bounds, supertypes, the inline-class
  underlying type) names the class's own parameters (`type_parameter_name`) and its members address
  them by id. An inner class addresses an outer class's parameter by id alone, so the outer name never
  enters the inner class's `d2`; a reader resolves the id through the outer class's own metadata, as
  kotlinc's `TypeDeserializer` chains to its parent. An application of an inner classifier has the
  opposite, use-site ordering: its flattened semantic arguments and source-spelling sidecars list
  the inner classifier's own arguments first, followed by the captured outer arguments. Its `Type`
  message keeps only the own prefix and recursively records the tail on `outer_type`; splitting the
  spelling sidecar at the same boundary preserves outer-argument abbreviations, type-use annotations
  and definitely-non-null flags. Tests: `tests/metadata_type_reference_e2e.rs`,
  `tests/inner_class_outer_tparam_e2e.rs`, `tests/metadata_inner_outer_type_e2e.rs`.
- Inline parameter modifiers (kotlinc 2.4.20): `ValueParameter.flags` bit 2 for `crossinline` and
  bit 3 for `noinline`, beside `DECLARES_DEFAULT_VALUE` (bit 1). Also written on a nullable
  function type and on extensions. Test: `tests/metadata_inline_parameter_modifiers_e2e.rs`.
- `tailrec` (kotlinc 2.4.20): `Function.flags` bit 11 on every tailrec function, top-level or
  member, whether or not the backend loops it. Test: `tests/metadata_function_flags_e2e.rs`.
- Parameter defaults (kotlinc 2.4.20): `DECLARES_DEFAULT_VALUE` only on a parameter whose own
  declaration writes the default. An override, or a member that only implements an interface
  function through a subclass's fake override, inherits the default for callers but does not
  declare it. Test: `tests/metadata_parameter_defaults_e2e.rs`.
- Enum supertypes (kotlinc 2.4.20): the declared interfaces in source order, then the implicit
  `kotlin.Enum<E>`. Test: `tests/metadata_enum_supertypes_e2e.rs`.
- Enum entry body (kotlinc 2.4.20): the `Enum$ENTRY` class is `k=1` with an `ENUM_ENTRY` kind and
  LOCAL visibility (flags 202 for a final body) and no constructor record. Its class id keeps the
  `pkg/Enum.ENTRY` spelling and is marked local (`StringTableTypes.local_name`), since kotlinc gives
  an entry's anonymous object no raw-name replacement. A class nested in the body is a local class
  (LOCAL visibility, no nullability annotations) whose id is `pkg/Enum.ENTRY.Nested`, and a
  signature naming it records its JVM descriptor. The body keeps its members' nullability
  annotations. Test: `tests/metadata_enum_entry_classes_e2e.rs`.

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
| `val z: Int = 0` (member) | 8710 | a zero literal counts too, though no field store is emitted for it; `1 + 2`, `val x: Any = 0` and `val x: Int? = 0` do not. Read from the declared initializer. Test: `tests/metadata_property_flags_e2e.rs` |
| `val b = run { … }` | omitted (=518) | computed initializer ⇒ no `hasConstant`; f11 elided at wire default |
| `const val c = 7` | 10758 | + `isConst`(1<<11); f100 = field entry ONLY (no getter method exists) |
| `val d get() = 5L` | omitted | f7 (getter_flags) = 70 = public·final·`isNotDefault`(1<<6); NO field entry |
| `var e: Double? = null` | 1798 | `isVar`(1<<8)+`hasSetter`(1<<10); field entry records desc `Ljava/lang/Double;` (boxed) |
| `lateinit var f: String` | 5894 | + `isLateinit`(1<<12) |
| `private val p1 = 3` | 8706 | visibility bits (f>>1)&7: INTERNAL=0, PRIVATE=1, PUBLIC=3; NO getter sig |
| `private var p3 = ""` | 1794 | neither getter nor setter sig: a private property's default accessors are never generated. f100 names exactly the accessors the facade declares, so a private property with a declared setter records only that setter. Test: `tests/metadata_property_flags_e2e.rs` |
| `internal val p2` | 8704 | getter sig present, unmangled |
| `var s1 … set(v){…}` | 1798 | f6 = setter value parameter `{name,type}`; f8 = 70 |
| `val String.doubled get()` | omitted | f5 receiver; f7 = 70; NO field entry |
| `val lz by lazy { … }` | 33286 | + `isDelegated`(1<<15); field entry `{name="lz$delegate", desc="Lkotlin/Lazy;"}` |

Accessor words (f7/f8) follow the class-property rule: each is written only when it differs from
the default word the property implies (its visibility, final modality). A declared or delegated
accessor sets `isNotDefault`, so a private property's declared getter writes 66 and a public one's
70. A `private set` writes the setter as private (66, `isNotDefault` when the property itself is
not private). A private property's declared accessors are private methods. The setter's value
parameter (f6) is recorded exactly when the setter is not the default one, under `value` or
`<set-?>` when no name was written, and it interns before the property's name. Test:
`tests/metadata_property_flags_e2e.rs`.

A property with one declared accessor still gets the other as the default method
(`var x = 0; set(v) {…}` has a public `getX`), and f100 names both. A bodiless `private set`
keeps the default setter, which is private and so never generated: f100 names only the getter,
f8 is still 66 with a `<set-?>` parameter, and another class in the file writes through
`access$set<X>$p`.

A delegated top-level property records `isDelegated` as a member property does. An ordinary
delegate's `JvmPropertySignature.field` names the `x$delegate` storage, interned after the accessors,
with its descriptor when the field's type maps differently from the property's (see
`JvmFieldSignature.desc` presence above). A singleton or constant delegate has no storage entry;
its generated `get<X>$delegate` method is interned before the accessors and recorded as
`JvmPropertySignature.delegateMethod` (f5). A top-level `lateinit` property likewise records
`isLateinit`. A private delegated property's accessors are private methods. Tests:
`a_delegated_top_level_property_records_its_delegate_field` and
`a_top_level_lateinit_property_records_lateinit`.

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
| `Handler` (`= (Cargo) -> Cargo`) | a USE of a function-type alias carries the right-hand side's component spellings on its expanded `Function1` arguments |
| `(count: Int) -> Unit` | the parameter's `Type` records `Type.annotation` `@kotlin/ParameterName` with one argument (`name` = string-table id, value `{type=8 STRING, string_value(f5)="count"}`), after the type's own annotations; d2 interns the class, then `name`, then the value |
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
  lowering — the same mechanism `fn_param_declared_nullable` already used. Property spellings use
  `(classifier identity, stable declaration order)`, not `(classifier, source name)`: ordinary and
  member-extension properties may legally share a name.

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
wrote. Package functions, member functions, constructors (annotation classes included) and a
`vararg val` constructor property all record the projected array. A primitive specialized array has no type argument and is recorded unchanged.

## `Property.flags` HAS_CONSTANT

Read from kotlinc 2.4.20's `FirElementSerializer.propertyProto`:
`isConst || (!isVar && canBeUsedForConstVal(fullyExpandedType(returnType)) && hasConstantValue(initializer))`.
`hasConstantValue` (`FirToConstantValueChecker`) accepts a supported literal, a string
concatenation whose arguments are constants, a `const val` (or Java `final` field) read, an enum
entry, an annotation constructor, `arrayOf` over constants, and a call in package `kotlin` named
in `NUMBER_CONVERSIONS` or `unaryMinus` whose dispatch receiver is constant. kotlinc's parser folds
`-3`, `+3` and `"a" + "b"` into literals before that. Measured the same in classes and at top level:

| initializer | HAS_CONSTANT |
| --- | --- |
| `val s: String = "s"`, `val u: UInt = 1u`, `val n: Int = -X` | yes |
| `X.toLong()`, `(-3).toLong()`, `0.toDouble()`, `1.0.toInt()`, `65.toChar()` | yes |
| `"$X"`, `"a${S}b"`, `"a" + "b" + "c"`, `+3`, a bare `S` | yes |
| `val a: Any = "t"`, `val c: CharSequence = "abcd"`, `val m: String? = "m"` | no |
| `(1 + 2).toLong()`, `1 + 2`, `!true`, `"a" + 1`, `"a" + S`, `S + "a"`, `+X`, `3u.toUInt()` | no |

## Accessors, setter parameters and member list order

Measured against kotlinc 2.4.20:

- `JvmPropertySignature.getter`/`setter` name the property's own accessor methods. A private
  property with default accessors has no accessor method, so it records the field alone, even when
  the class declares `getValue(reference, property)` or an exact `fun getCount(): Int` /
  `fun setCount(next: Int)`. Those stay `Function` records.
- `Property.setter_value_parameter`'s type addresses the property's own type parameters by id
  (`Type.type_parameter`), while the return type and receiver name them
  (`Type.type_parameter_name`). The setter is a declaration of its own. This holds for package and
  class properties alike.
- `Class.property`, `Class.function` and `Class.type_alias` serialize in the order the records were
  built, which is the d2 interning order. With `var <M> Slot<M>.member` declared before
  `val later`, both lists put `member` first.

## `Property.flags` HAS_CONSTANT

Read from kotlinc 2.4.20's `FirElementSerializer.propertyProto`:
`isConst || (!isVar && canBeUsedForConstVal(fullyExpandedType(returnType)) && hasConstantValue(initializer))`.
`hasConstantValue` (`FirToConstantValueChecker`) accepts a supported literal, a string
concatenation whose arguments are constants, a `const val` read, a provider-published Java `final`
field read (including one without a `ConstantValue` attribute), an enum entry, an annotation
constructor, `arrayOf` over constants, and a call in package `kotlin` named
in `NUMBER_CONVERSIONS` or `unaryMinus` whose dispatch receiver is constant. kotlinc's parser folds
`-3`, `+3` and `"a" + "b"` into literals before that. Measured the same in classes and at top level:

| initializer | HAS_CONSTANT |
| --- | --- |
| `val s: String = "s"`, `val u: UInt = 1u`, `val n: Int = -X` | yes |
| `X.toLong()`, `(-3).toLong()`, `0.toDouble()`, `1.0.toInt()`, `65.toChar()` | yes |
| `"$X"`, `"a${S}b"`, `"a" + "b" + "c"`, `+3`, a bare `S` | yes |
| `val a: Any = "t"`, `val c: CharSequence = "abcd"`, `val m: String? = "m"` | no |
| `(1 + 2).toLong()`, `1 + 2`, `!true`, `"a" + 1`, `"a" + S`, `S + "a"`, `+X`, `3u.toUInt()` | no |

## Class supertypes: only what source declared

`Class.supertype` (f6) lists the DECLARED supertypes in source order, wherever the superclass is
written: `class C : I, Base(), J` writes `I, Base, J`, and `d2` interns them in that order. Delegated
interfaces, parenless superclasses, local classes and anonymous objects follow the same rule. An
undeclared `kotlin/Any` is never recorded — `class Holder<T>(val t: T) : Iface` writes one supertype record, not two.

This is a trap for a generic class: its supertype list comes from the recorded generic signature
(`ir.class_signature(..).supers`), which ALWAYS materializes the superclass position because a JVM
class `Signature` attribute must name a superclass, holding `kotlin/Any` when none was declared. The
metadata emitter drops that implicit entry (`jvm/ir_emit.rs`), leaving both the generic and the
non-generic shape with a superclass slot exactly when one was declared.

`DeclaredSpellings::supertype_spellings(has_declared_superclass)` aligns the typealias spellings to
that list. The flag cannot be inferred from the spellings themselves: a declared superclass that
named no alias spells nothing, yet still occupies the slot, so only the emitter knows. Getting it
wrong shifts every abbreviation onto the neighbouring supertype.

Both lists lead with the superclass. The emitter then moves it to the slot that resolution recorded
in `ResolvedClassifierHeader` and lowering published as `IrFile::class_superclass_positions`, so the
spellings move with it. Tests: `tests/metadata_supertype_order_e2e.rs`.

## Prepared KLIB inline return targets

Successful prepared dependency expansion retains external-inline provenance on its final common IR
root, including the operand prelude when present. This is an IR fact, not a metadata wire-format
change; the real kotlinc-native producer test requires a distinct marked region for each call.

The selected Native stdlib's prepared `requireNotNull` and `checkNotNull` bodies contain
branch-local returns to serialized returnable-block symbols. Those symbols are control-flow
identities, not local-value declarations or returns from the containing callable. Common lowering
keeps a lexical target stack for every block, including the sole-tail-return fast path, and uses
anonymous result storage only for structural frames. Repeated enclosing target declarations,
reads of a target as a value, and value frames with reachable fallthrough reject explicitly.
The existing IR decoder and metadata wire format are unchanged. Structural tests live in
`klib_lowering::tests::returnable_blocks`; `tests/box_regressions/multimodule/inlineNullCheckReturnTargets.kt` supplies one shared
JVM/Native/Wasm declaration graph with reference/scalar results, exact failure messages and
single evaluation of its argument.

Prepared calls retain the callee's recorded extension-receiver parameter coordinate; it does not
become a dispatch receiver merely because the body came from a KLIB. Native's existing range-builder
roles consume either a separate receiver and one argument or the serialized parameter-zero receiver
and one argument through the same representation path. Other layouts reject explicitly. This adds
no declaration role or metadata wire format; the selected provider fact still owns the operation.

The caller's nameless initializer context is a lowering fact as well. It is inherited through
source lambdas, not named local functions, so an absent source callable spelling in an initializer
does not reject an otherwise valid inline copy. This adds no metadata field or synthetic name.

The target stdlib's exact zero-argument `CharSequence.trim(): CharSequence` extension is published
with `CompilerIntrinsic::TextTrim` only after the provider authenticates the selected stdlib and
its complete receiver, parameter, result and modifier shape. An identical ordinary dependency
does not acquire that operation. Native consumes the fact and the recorded receiver coordinate,
and proves contiguous String/StringBuilder storage before invoking its existing trim runtime.
Neither the serialized signature nor the callable is reconstructed from a backend spelling.

## KLIB accessor declaration modifiers

Property getter and setter words are decoded independently, including visibility, modality,
non-default, external and inline status. An absent word takes only the shared property prefix
(bits 0-5); an explicit zero remains zero. Property external (bit 14) is separate from accessor
external (bit 7). Accessor inline is bit 8 and non-default is bit 6. Inline or external on a
default accessor is rejected as inconsistent metadata. The semantic declaration keeps each
accessor's own annotations (modern fields 15/16, BuiltIns fields 152/153, KLIB fields 177/178);
`InlineOnly` obligation comes from the selected accessor, not the property's annotation list.
Each accessor also carries the property's declaration-owned reified formal ordinals. These are
existing schema facts, not new intrinsic markers or a metadata format change. This producer fix
alone does not enable ordinary dependency-body or inline-accessor emission.

## Serialized closure capture ownership

Closure frames bind exact arena-local value symbols against their enclosing declaration's value
environment. A lexical scan includes nested closures, while each nested lowering starts a fresh
parameter/local/control-flow frame. The lifted declaration publishes its capture prefix,
parameter identities, receiver ledger and private visibility; rollback removes these side facts
with the appended functions. The serialized expression's source-form marker distinguishes a
literal from an anonymous function. Full source naming/debug origins are not reconstructed from
generated names or invented for dependency bodies. Official-producer tests cover raw bodies,
prepared inline copies and runtime consumption through the existing Native closure ABI.
The emitted-callable walk extends only selected expression roots with reached, supported closure
implementations. It deduplicates nested implementations and projects every call through the same
producer-origin graph as direct calls. Defaults and unrelated local declarations remain outside the
root set, and intrinsic-authority inventories stay direct-only. The prewalk checks outer function
class shape independently of generic argument binding; body lowering retains full capture/header
validation in its enclosing frame. Imported constructor facts can therefore be prepared without
claiming a target implements the imported classifier's storage or allocation.

## Imported class initialization ownership

The selected constructor freezes its declaring class's kind, modality, value/inner/expect flags
and exact primary-constructor signature beside the existing producer/class identity. The primary
signature is retained before visibility filtering, so a private primary constructor cannot turn a
secondary constructor into an apparent primary. The raw class header authenticates that complete
contract. A shared positive plan admits ordinary final classes with a sole primary constructor,
an exact `Any` delegation and their own instance initializer; non-admitted classes remain target
leaves, and malformed admitted declarations decline without trying another lowering path.

Field storage uses serialized field symbols, types and typed field flags. Explicit initialization
stores run in serialized member order alongside anonymous initializer blocks. Full constructor
arity belongs to `ctor_args`; plain frame arguments do not imply constructor-backed fields, so
the generated class records zero implicit constructor-field stores. A private instance helper
retains the real receiver slot and the exact constructor value symbols. Field access is admitted
only within this authenticated initialization frame. The ordinary Native layout, allocation and
closure ABI consume that common IR; no field layout is guessed from argument positions.

Preparation follows only the initializer edges of this same positive plan. Class and helper
publication share the body's rollback checkpoint. Until qualified class facts and constructor
exports connect independent units, emission explicitly rejects equal source classifier names,
different producer identities and repeated materialization in separate support units before
backend emission or export publication. This first slice executes dependency factories; it does
not yet establish direct source construction, imported accessor/member dispatch or full #2621.

## Ordinary KLIB generic declaration headers

The KLIB body contract separately freezes known implementation modifiers from each exact signed
function or accessor. Constructors and providers without such a contract keep `None`, not an
invented non-inline default. Known modifiers survive provider realization and the checked backend
handoff, participate in declaration-contract equality, and authenticate raw function headers.
Serialized IR FunctionFlags use inline bit 10, external bit 12, suspend bit 13 and modality bits
4-5; these happen to share metadata function encoding, not the shorter accessor encoding.
Property external status stays independent of the accessor's own external bit. Ordinary-body
admission must require known facts and veto both external statuses; this contract does not itself
enable a new emission root category.

The ordinary-body emitter consumes these facts only for exact checked uses in structurally
emitted bodies. It freezes provider realizations for transitive body-bearing declarations before
lowering and does not allow an unselected callee to bypass the metadata contract via a bare raw
header. The top-level/extension declaration-frame capability is independent of scalar shape,
intrinsic adjacency and stdlib origin. Instance-storage capabilities remain target-owned.
Target-specific default-wrapper and enum-entry-only roots still require a later inventory slice;
the emitter does not scan all default/inline-template expressions as an approximation.

Provider header evidence remains distinct from checked source-call obligations. Adding the exact
declaration of a transitive primitive does not require an out-of-line export or suppress its
existing common-IR builtin lowering. Conflicting declaration evidence still declines even when
no source call selected that identity.

Graph preparation asks the common body's exact builtin call-disposition capability before
requiring a provider declaration or traversing its implementation. The same decision drives
actual call lowering; compiler-owned operations do not acquire spurious body/header obligations.
Selected source declarations and ambiguous contracts cannot use this capability, and already
emitted exports remain external edges. This is positive builtin recognition, never recovery from
a failed provider lookup. Non-builtin bodyless declarations still require their exact provider.

Raw call-header authentication distinguishes an external declaration from an emitted support
body. A bodyless external member retains its dispatch operand and authenticates its classifier
against the selected declaration owner; it does not need the singleton receiver-erasure proof
required to emit a member support body. Both purposes still check the same raw modifier, generic,
parameter-role and result contracts. Target intrinsic leaves use the external declaration boundary.

The KLIB provider retains each formal's exact normalized semantic type alongside the full generic
signature. Both use the same metadata identities, enclosing bounds and bound-normalization helper;
the frozen backend handoff copies the formal types unchanged. Full bounds can contain deeper
finite self/forward references than the formal's primary bound, so consumers must not rebuild
formal types from `GenericSig.formal_bounds`. Ordinary header lowering binds all serialized
symbols first, then compares every bound and the complete selected parameter/result contract.
Prepared-inline erasure is not accepted by this path.

Native linkage retains the actual emitted IR signature beside each exact dependency export.
Provider generic signatures and checked external-formal substitutions validate source operand
roles separately; specializing a call never changes the published symbol's physical signature.
No serialized metadata format or formal identity is rewritten to achieve ABI coercion.

Common IR separately records an inner generic call's declared result and the checked result of
its enclosing conversion. Wasm now consumes that existing declaration-result provenance when
the inner call deliberately has no logical type; this changes no KLIB metadata or type identity.

Serialized type-parameter flags use the active Kotlin `TypeParameterFlags` encoding, which
delegates to `IrFlags`: annotations at bit 0, variance at bits 1 and 2, and reification at bit 3.
Invariant non-reified formals have flags 4; invariant reified formals have flags 12. Ordinary
function headers require invariant formals and exact raw/provider reification agreement before
rejecting standalone emission of reified bodies. This changes no metadata wire format.

## Authenticated KLIB collection classifier roles

The ordinary stdlib `Iterable<T>.collectionSizeOrDefault(Int): Int` helper receives a normalized
implementation fact only after authenticating the selected stdlib, its single declaration-owned
formal with the nullable Any bound, the complete extension signature and modifiers. This does not
invent a TypedIntrinsic annotation or make other ordinary bodies eligible as support roots. Native
consumes exact property override identities to expose program Collection.size through a typed
descriptor thunk; classifier roles distinguish Collections from plain Iterables.

The selected stdlib's size-only `Array<T>` constructor also publishes a semantic allocation role,
after checking its class formal and full constructor signature. The role survives ordinary
constructor realization and lets prepared inline bodies lower allocation to the same `NewArray`
operation as source arrays. It is not an annotation synthesized onto the serialized declaration,
and an ordinary dependency with the same spelling has no authority to publish it. Getter calls in
prepared bodies retain the exact selected property's normalized name independently of the
accessor's physical spelling (`Collection.size` versus `<get-size>`). No metadata fields change.

Collection kind and read-only/mutable face are normalized provider facts, not new metadata fields.
The compilation-selected platform stdlib authenticates the sixteen canonical collection interface
headers; publication validates their interface kind, own/total type-parameter arity, and absence of
an enclosing instance before recording the existing `MappedCollection` role. An identical name in
an ordinary dependency grants no role. Consumers read that fact rather than infer it from spelling.
Native 2.4.21 metadata declares one own parameter for iterable/iterator/collection/list/set faces
and two for map/entry faces, including the static nested `Map.Entry` and `MutableMap.MutableEntry`.
This changes no metadata wire format. Tests: `klib_libraries::tests::classes::collection_roles`.

## Authenticated KLIB primitive headers

KLIB body-callable views retain the effective compiler operation from either the callable or its
normalized member realization. Instance members also freeze their exact declaring module/class
signature and the original class-formal types, full bounds, variance and own/captured count from
the signed classifier. This is provider-owned header evidence, not a later classifier-name lookup.
Common primitive lowering binds the actual raw class symbols and checks the serialized receiver,
accessor/property membership and full function header before using that evidence. It neither
publishes private implementation functions nor broadens ordinary instance-body emission. No wire
format or Native runtime layout changes. Tests: `klib_lowering::tests::primitive_calls`.

Constructors retain the same class contract through provider lookup and both frozen views. The
join requires the exact declaring module and complete constructor signature, including its mask.
Class-owned formals and bounds stay in that contract; they are not constructor-owned function
parameters. The invocation result remains `Unit`, without a function generic signature, independently
of source inference's constructed type. The body view separately retains known constructor-default
flags, vararg position, visibility and semantic role. Empty canonical metadata defaults expand to
the constructor's exact parameter count; unknown non-KLIB defaults remain unknown. Shared raw
authentication checks constructor kind and slot, exact class application and all parameter facts
before ordinary or prepared bodies instantiate class arguments and their complete upper bounds.
The executed-body inventory includes construction without treating serialized constructor bodies
as top-level support exports or granting intrinsic-root authority.
Missing optional provider evidence does not erase a proven raw constructor declaration: the
body declines its realization, while contradictory kind/handle evidence remains a hard mismatch. Tests:
`klib_lowering::tests::constructor_contracts`, `::constructor_calls`.

Wasm dependency publication retains the actual function's declared semantic ABI beside its exact
module-qualified identity. Selected property operations use frozen getter/setter identities to join
the same export registry as ordinary calls; they never invent property globals or recover accessor
spellings. Native and Wasm share receiver-coordinate authentication. Property IR currently carries
no checked generic substitutions, so generic accessors remain explicitly unsupported by this route.

## Declaration-owned KLIB vararg coordinates

Metadata value-parameter vararg indices include context parameters but exclude an extension
receiver. The provider freezes the declaration index in its normalized parameter order, inserting
the extension offset at the context boundary. Associated companion receivers remain a separate
contract and are accounted for only when authenticating their actual serialized slot. The frozen
body view never reads the callable's call-site-specialized `vararg_elem`/`vararg_index` fields.
The declared packed parameter supplies its element; raw element markers use the same formal binder
and must agree exactly. Prepared copies keep their existing checked declared/erased type boundary.
No serialized layout or runtime ABI changes. Tests: `klib_lowering::tests::varargs`.

## Compiler-plugin declarations in a KLIB

kotlinx.serialization is split like kotlinc's plugin: its frontend part declares what Kotlin code
can name, and its IR part fills bodies and adds IR-only helpers. krusty declares the frontend part
at the backend handoff, before the declaration records are built, so every target describes the
same declarations; each backend completes them afterwards.

For `@Serializable class Foo(val a: Int, val b: String = "x")` kotlinc-native 2.4.20 writes, beside
`Foo`'s own members:

- `Foo`: an `internal` secondary constructor `(seen0: Int, a: Int, b: String?,
  serializationConstructorMarker: SerializationConstructorMarker?)`;
- `Foo.$serializer`: a `@Deprecated(HIDDEN)` object implementing `GeneratedSerializer<Foo>` with a
  private constructor, `childSerializers`, `deserialize`, `serialize` and `val descriptor`;
- `Foo.Companion`: a private constructor and `serializer(): KSerializer<Foo>`.

`nestedClassName` lists `$serializer` before `Companion`, and the fragment's classes follow that
order. A plugin-generated class and its members carry no file extension (`classFile` 175,
`functionFile` 172, `propertyFile` 176): no source file declares them. `write$Self` is not a
declaration: the JVM adds it to its own `@Metadata` record after the handoff.
`klib_class_surface_e2e::serialization_plugin_declarations_match_kotlinc_native` compares the whole
fragment with kotlinc-native's bytes.

## Authenticated collection storage construction

Package-function normalization can freeze an empty/singleton/packed list-storage plan only for
the selected stdlib's full generic declaration. It checks the formal's complete bounds, exact
parameter/result uses, declared vararg position and modifiers before call-site substitution.
The JVM provider supplies its metadata typealias expansion for `ArrayList`; KLIB supplies its
declared classifier. This adds no Kotlin metadata field or wire encoding. Frozen callable facts
carry the typed plan, and Native uses it instead of recovering an operation from a facade name or
physical parameter. Dependency graph admission also requires the target's positive implementation
capability and complete serialized/header agreement. Body-bearing target leaves retain their raw
external signature and generic formals; they are not previously emitted body exports. Neither this
capability nor its bookkeeping adds a metadata encoding or publishes a private implementation.

Native's typed dependency plan also consumes the provider's existing authenticated `StringPlus`
operation for both member and nullable-extension declarations. This publishes no new metadata fact:
raw headers still prove the exact module, receiver roles, argument and result. A serialized extension
retains its receiver coordinate; target emission does not recover it from a callable spelling. The
positive plan does not admit ordinary library algorithm adapters or bypass declaration checks.
Both member and package normalization require the compilation-selected stdlib input's existing
authority before publishing StringPlus. The identical archive opened as an ordinary dependency
receives no such operation, including either frozen callable lookup path; matching bytes, module
names and complete signatures do not grant platform authority.

## Authenticated Throwable realization

KLIB classifier and property records retain the selected platform-stdlib input's existing archive
authority before constructor/accessor identities are published. The exact base `Throwable()` and
`Throwable(String?)` signatures and immutable `message: String?` member may receive typed compiler
realizations; an ordinary dependency with identical metadata may not. This does not rewrite Kotlin
metadata or change source overload selection. Wasm owns the GC fields and exception tag; common IR
keeps the selected constructor/getter identities and typed `Try` / `Throw` nodes.

## Inline-body migration outcomes

The approved #2459 Native/Wasm outcome reset changes no metadata encoding or declaration
authority. It records known implementation debt for small follow-ups, pinned to exact loaded
manifest blobs and effective releases. JVM/applicability and fatal-outcome policies remain
unchanged; see [KLIB inline migration](KLIB_INLINE_MIGRATION.md).

## Wasm nullable assertions

Nullable assertion emission consumes the existing checked operand and result types; it adds no
metadata field or nullability rewrite. A Wasm reference-to-scalar assertion declines until its
unboxing and typed null failure have a complete target realization.

## Authenticated String concatenation declarations

The member `String.plus(Any?): String` and nullable extension `String?.plus(Any?): String`
require the compilation-selected stdlib's existing archive authority before receiving the shared
StringPlus operation. Loading identical archive bytes as an ordinary dependency does not grant it.
Member function normalization now precedes callable identity publication. The target-free body
view projects the normalized member operation just as it projects a package-function operation,
so source candidates, opaque callable lookup and module-qualified serialized-declaration lookup
retain the same operation.
No Kotlin metadata encoding or declaration identity changes. The actual Native stdlib regression
checks both declaration forms through all three views, with and without platform authority.

## Reference-array cast bound evidence

Dependent callable-bound validation uses semantic formal identities and the existing completed
binding map. Deferring unresolved call-owned relations changes neither generic declaration
metadata nor emitted signatures; a reference-built dependency and a source declaration use the
same rule. Caller-owned formals and concrete upper bounds remain immediately constraining.

Compatible JVM array casts read the existing IR generic signatures and exact recorded non-local
and lambda bound declarations. They introduce no Kotlin metadata encoding or provider lookup.
Expected components use declaration-primary erasure; actual components retain the complete
intersection of declared bounds. Backend wrapper names do not replace Kotlin classifier identity.

## JVM signature segment evidence

JVM `Signature` syntax now reaches an application builder as the complete written segment sequence,
with physical identities and optional argument lists. Parsing no longer overwrites outer arguments
before that construction boundary. The standalone policy still keeps innermost arguments and marks
discarded outer arguments inexact for fields. Class/array erasure is computed from syntax and checked
against the complete field descriptor independently of the builder's semantic result.

This changes no Kotlin metadata wire fields or raw-type flags. Declaration-backed raw/captured
construction remains separate from this syntax boundary.

## JVM constructor boxing consumption

The existing constructor declared-parameter sidecar retains the selected declaration's erased
operand slots independently of call-site substitution. The JVM now applies its shared post-boxing
reference coercion at those physical slots. No Kotlin metadata encoding, formal identity, source
type or declaration normalization changes. Dependency captured receiver specialization is owned
by #2593 and is not reconstructed by the boxing emitter.

## Receiver-bound constructor signatures

Receiver-bound constructor candidates retain the full classifier argument layout, including captured
enclosing arguments, even when a source provider's provisional constructor result contains only its
own arguments. Captured declaration identities are substituted from the selected outer application;
they are not constructor inference variables. Ordinary callable-owned formals remain independent.
The enclosing declaration's normalized argument bindings are re-keyed onto the inner classifier's
captured suffix. A hidden captured parameter and a same-spelled own parameter retain their distinct
provider identities, including Java declarations; projection and dependent-star bounds stay intact.
Complete capture layouts provide declaration-order correspondence; selective layouts retain exact
semantic outer identities. Only present normalized bindings enter the captured context, so an
unapplied outer does not acquire invented type arguments.
The same complete result reaches inferred function signatures and rejected-call recovery. This
changes no metadata schema, physical constructor descriptor or enclosing-instance ABI.

Receiver-bound aliases carry the already-selected alias expansion and its declaration signature on
the constructor candidate. Alias formal order governs written type arguments independently of the
target classifier's own-plus-captured layout. Inferred return publication uses the selected named
argument mapping and preserves the alias abbreviation. Independent underlying constructor formals
retain their semantic identities and declaration-specialized owner signature for diagnostics; they
are not added to the alias's inference variables. No metadata schema or physical descriptor changes.
Receiver-call inventories retain classifier and callable facets from the same declaration read;
object-import singleton projection preserves those classifier and alias facts as well.
Alias applicability substitutions do not replace the target constructor's original declaration
parameters: generic value-class slots remain boxed, while concrete slots retain their carriers.

Selected alias constructor references keep the constructor's original declaration parameters apart
from the synthetic extension lookup receiver. Projection of that facet removes only its known
receiver slot; physical descriptor/carrier evidence and stable constructor identity remain intact.
Inferred source-call results retain receiver projections through declaration-signature publication
using the common output-position specialization. This changes no Kotlin metadata wire schema.

Selected member records also retain pre-substitution declaration parameters in full source order
(contexts, extension receiver, values). Metadata function normalization publishes this fact before
specialization; callable/member projection and constructor publication carry it without recovering
it from the specialized signature. Constructor outer-instance and default-marker slots remain
physical inputs. This is an in-memory declaration fact, with no Kotlin metadata wire change.

Failed constructor argument recovery consumes those same normalized declaration signatures and
receiver bindings. It does not infer function shape from classifier spelling or recover declarations
from rendered diagnostic text. Nominal function classifiers normalize through provider evidence;
vararg elements and anonymous receiver parameters retain their declared semantic slots. Alias
candidate diagnostics keep the unspecialized declaration signature while input recovery retains
independent substitutions. No wire model or constructor ABI changes are required by #2587.

Receiver admission distinguishes active call-owned inference variables from rigid declaration
parameters. A satisfiable trial specializes both receiver sides before strict comparison without
publishing new declaration facts. Alias expansion, selected constructor identity and retained
declaration parameters remain unchanged; no metadata wire or physical ABI change is involved.

## Projected receiver input diagnostics

Projected receiver input restrictions are compiler-internal facts from the shared position-aware
specialization walk. They change no metadata type, physical descriptor or provider identity.
Receiver constraints remain distinct from expected-result constraints, and nested projections
retain their original declaration shape; the direct-input diagnostic does not invent a captured
classifier or globally rewrite stars.

## Annotation classifier nesting

JVM annotation interfaces use the same retained `InnerClasses` inventory as other declared
classifiers. Declared children and enclosing rows remain discoverable without a method or element
referencing them. This changes the JVM class table, not Kotlin metadata or annotation values.

## Declaration-result source provenance

Declaration-result conversions retain the checked source position on their original producer and
selected operand-bound call. Existing positions, return endpoints and inline origins remain intact;
zero source positions stay absent. This is common debug provenance for target emission and changes
neither semantic substitution nor a metadata wire format.

## Initializer inline debug ranges

Class-initializer entry establishes the implicit callable frame. Initializer expressions record
their local-variable ranges at nested lexical boundaries, and each store restores its source line
after an inlined value. Class-initializer table curation preserves intentional marks at distinct
offsets, including returns to the same source line. The JVM uses these debug points to preserve
inline markers and closing anchors; Kotlin metadata and constructor descriptors are unchanged.

## Nominal function subtype evidence

Provider-normalized function classifiers supply their declared generic supertypes to the common
subtype relation. A typed function signature supplies its canonical classifier application, and
the hierarchy walk checks the nominal value against that application. Reflective function types
retain their nominal identity; no new metadata payload or physical JVM descriptor is introduced.

## Generated constructor-default source marks

The JVM class writer clears constructor line tables before the owning emitter publishes its
curation. Default-stub emission always supplies its recorded expression marks, including an empty
set when no source positions exist. A zero owner declaration line suppresses entry/closing marks
without discarding retained default-expression lines. Kotlin metadata and default payloads are
unchanged; complete class artifacts pin the resulting debug attributes and constant-pool order.

## Superclass-argument receiver availability

An inherited property declaration remains evidence for a name even while its dispatch instance is
unavailable. The checker probes the normalized property model on that exact declared receiver to
report the initialization diagnostic, without publishing a getter, field read, or capture. Inferred
property result demand follows receiver selection; it cannot turn an instance member into a local
value. No metadata shape or backend ABI changes accompany this frontend correction (#2658).

## Rejected constructor lambda constraints

Normalized constructor signatures and the canonical source-argument mapping retain postponed
function-expression expectations when an explicit class argument violates its bound. Reporting
uses the same typed failure evidence for source and reference-produced dependency declarations;
it does not reconstruct parameter types from constructor text. Metadata encoding and callable ABI
are unchanged. Callable-reference publication retains its selected declaration's diagnostic
projection before invocation adaptation. A rejected constructor constraint consumes that same
projection and the mapped functional types, retaining the constructor heading, bound error and
reference-candidate error in order. Top-level, member, extension, local and constructor providers
share this checker boundary; reference revisits discard previous evidence. No diagnostic text
becomes metadata, a callable identity or a backend target (#2776).
The selected declaration also determines receiver-constraint wording. Constructor references retain
their declaration diagnostic before adaptation: generic formals and the logical classifier result
come from normalized constructor/classifier facts, while aliases retain their selected alias and
outer-owner signature. The physical constructor return remains unchanged.

## Anonymous superclass closure capture ownership

Constructor-use inventory distinguishes candidate storage discovery from checked publication.
An admitted retained lambda keeps lexical dependencies in its anonymous class; ordinary forwarded
super arguments remain construction-site expressions. Final closure admission uses the same
checked lowering/type facts as super-argument disposition. Existing lexical capture identities,
shared-cell decisions and FIR constructor-prefix coordinates supply the resulting fields and
closure parameters. No metadata naming, new capture identity or backend recovery is introduced.

## Serialized getter declaration contracts

KLIB normalization tags a getter or setter from its actual property slot, retaining the property's
opaque identity and decoded name. This KLIB-only facet survives both declaration projections and
participates in declaration-contract equality; it grants no execution capability by itself.
Ordinary getter linking authenticates the serialized property group and class header, including
the raw dispatch classifier's signature mask and complete own-formal receiver application.
Declared getter results remain distinct from the checked read's applied result. Native's runtime
leaf capability reuses its existing accessor signature plans and does not import property bodies
or reconstruct class storage. No metadata wire format changes.

## Compound member singleton identity

Checked FIR already records singleton declaration identity in `SingletonValue`. Compound member
updates reuse that identity through `CompoundMemberReceiver::StableRead`, preserving the selected
getter/setter and source facts. Repeated reads initialize the same declaration once; runtime calls
and mutable receiver reads remain explicit snapshots. No metadata payload or compiler/plugin wire
field changes are needed. Full raw JVM artifacts pin the eliminated receiver-local storage.

## Applied member input declarations

`FunctionInfo` retains a common declaration-input facet before nominal specialization: the raw
logical signature, exact receiver bindings, and complete owner-bound lists. Member extensions,
inner constructors and checked body-local methods preserve the same evidence. Suspend inputs use
the existing logical continuation normalization. Applied callable/physical parameter vectors keep
their existing ABI and packing roles; the facet does not add class formals to callable arity.

## Wasm constructor-prefix roles

Wasm constructor admission consumes `IrCtorArg`'s forwarded-super ordinal and parameter provenance
separately from the total constructor-prefix count. Only field-free ordinary forwards are admitted
by this extension; source metadata and capture storage retain their existing contracts.

## Receiver binding and safe-cast storage

Checked receiver coordinates and declaration paths authenticate own receiver parameters and lifted
receiver captures. Common lowering publishes their immutable binding-read fact on the resulting
`GetValue`; it does not infer stability from a slot number or the referenced object's mutability.
The existing cloning contract carries that fact. Shared-cell reads remain mutable semantic values.
For a marked safe-cast snapshot only, the JVM retains a reference cell's erased element slot through
the type test; source locals, primitive cells and semantic metadata keep their original types.
`implicit_receiver_stability_e2e` checks complete class inventories and bytes against Kotlin, then
compares execution of the same whole programs. No metadata wire format or capture ABI changes.

## Package property annotation records

Package property annotation presence follows checked PROPERTY/FIELD destinations. Getter, setter
and setter-parameter applications keep independent accessor flags; their source prefix does not
make the property's own metadata annotated. Optional expectations and SOURCE retention keep the
same policy as each checked declaration partition.

SOURCE presence survives independently of its dropped payload and false metadata annotation bit;
an erased optional expectation gains no source carrier. JVM property markers consume that retained
presence. An annotated default setter writes its value-parameter metadata like a class property's
setter, and parameter annotation payloads intern before the physical method body.

Package properties retain checked PROPERTY and FIELD payloads under their stable property identity;
metadata uses the same declaration flag/record model as class properties. JVM records use fields
14 and 34; KLIB records use 170 and 181. Accessor strings intern before the property records. KLIB
file extensions intern before annotation records, while constant payloads intern afterward. The
official Native property-message golden includes both integral and string constants to retain this
ordering independently of JVM marker realization.

## Annotation argument order

An applied annotation retains declaration-ordered element values and the checker's written indices
separately. The shared metadata encoder emits written values first and any materialized unwritten
values afterward, recursively through nested annotations and arrays. A provider or generated
application without source provenance keeps its stored order. JVM annotation attributes continue
to use the declaration-ordered values. The complete annotation-order fixture covers declaration,
constructor, function, parameter and property metadata alongside grouped and explicit vararg arrays.
Method parameter annotation payloads intern at the JVM header boundary before body constants,
with all visible parameters before invisible parameters and their nullability annotations.
The parameter-payload oracle compares the entire module, including ordinary members, static
companion forwards and interface compatibility holders with mixed retentions and nested values.

## Serialized Elvis provenance

An `ELVIS` block supplies an immutable compiler variable followed by a two-branch conditional
without its own origin. The selected equality signature and both reads must name the same exact
temporary/null proof; names never correlate these declarations. Declared temporary storage remains
separate from the present branch's proven type. Common IR retains Elvis provenance with the absent
source-line sentinel; no synthetic source line or metadata wire change is introduced.

## Unsigned constant value messages

`annotation_value_pb` writes field 10 = 1 for the four unsigned `IrConst` variants. The kind remains
BYTE, SHORT, INT or LONG and `int_value` remains the zigzag encoding of its signed carrier, including
high-bit values. Signed constants have no flags field. Recursive annotation and array values use
the same encoder. The complete compile-time-value messages of the pinned official Native producer
are the byte oracle; this does not infer unsigned identity when an earlier semantic model lost it.

## Checked unsigned annotation identity

The common annotation value model retains all four unsigned widths. Checked source literals and
constant references publish those values; the latter interpret bits using the constant's semantic
type before applying the declared element width. Common IR keeps the matching unsigned constant.
The metadata type-annotation adapter accepts exactly `IS_UNSIGNED` on BYTE, SHORT, INT and LONG
with valid signed carriers, and rejects other flags and incompatible kinds.

KSP protocol 16 transports four unsigned value tags. Its JVM adapter boxes their declared carriers
as Kotlin unsigned wrapper objects, matching KSP2's runtime classes and full values. The class
reader and build ABI fingerprint remain physical JVM contracts. Raw dependency annotation-default
normalization remains a declaration-type-owned provider follow-up.
