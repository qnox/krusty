# Streaming FIR coverage matrix

This is the landing inventory for the streaming FIR migration. The compiler-enforced source of
truth is `src/fir/coverage.rs`: its matches have no wildcard arms, so a new parser AST variant
cannot land without updating this inventory.

The status in this initial matrix is deliberately honest:

- **AST + side tables** means the existing checker handles the form and records decisions in
  `TypeInfo`, while `ir_lower` still consumes the AST.
- **FIR pending** means no checked streaming FIR node exists yet.
- **Parser-only diagnostic** is reserved for syntax that is represented solely so a semantic
  consumer can issue an exact error.

No form is marked FIR-complete until lowering consumes checked FIR without consulting AST or
`TypeInfo` side tables.

The landed signature infrastructure now provides stable declaration identities, a packed
`SignatureGraph`, inferred-declaration-only graph insertion, demand-driven memoized solving with
cycle detection, and a pending-free publication boundary. Its semantic evaluator is intentionally
split into one exhaustive structural graph walker and a resolver/checker adapter interface. Calls,
members, invokes, expected types, joins, nullability and substitutions all cross that interface;
the graph module contains no candidate or inference rules. Production wiring must implement the
adapter with the ordinary resolver/checker rather than a reduced expression typer.

The checked-FIR ownership schema is also present: pending-free expression types, stable selected
callable/property/control targets, final receiver/argument/default/vararg/substitution decisions,
source and synthetic origins, an inline-only body store, and a consuming ordinary-body sink. These
are memory and phase boundaries, not a claim that AST-to-FIR conversion has landed. Rows below stay
**FIR pending** until the production checker constructs these nodes and common lowering consumes
them without the AST or `TypeInfo`.

`ResolvedModuleIndex` now also owns a compact semantic header for every stable declaration plus a
pending-free classifier graph containing qualified classifier identities, selected generic
superclasses, and direct interfaces. Visibility, declaration flags, and ownership therefore survive
header-syntax destruction without retaining lookup spellings or parser IDs.

Pass-1 inventory now interns declaration spellings strictly as temporary lookup inputs and packs
package/import paths separately from stable declaration anchors. A shared header-syntax arena also
copies every non-local explicit type, parameter/default-presence fact, type parameter/bound,
receiver, supertype, constructor shape, and type-alias target before the file AST is dropped. It has
no expression/statement/declaration-arena IDs and does not walk bodies. Header finalization drops
the syntax, spellings, and scopes, publishes the anchors into `ResolvedModuleIndex`, and emits a
Pass-1-only executable inventory. It is partitioned from resolved callable flags so Pass 1 can
prepare all inline FIR before Pass 2, then it is destroyed. Pass 2 reparses declaration units in
source order and enumerates ordinary bodies directly from the one live AST.

Explicit-header checking crosses one `ExplicitHeaderSemantics` adapter: the streaming layer only
routes callable, property, constructor, classifier, and type-alias shapes. The adapter must invoke
the ordinary resolver/checker and return pending-free signatures or owned diagnostics. Expression-
inferred functions/properties are deliberately skipped there and remain the only lazy solver states;
an explicit classifier/type-alias failure independently blocks signature finalization.

Production `analyze_source_set_impl` now feeds the compact builder as each file is parsed. Stable
header identities perform multiplatform expect/actual selection (including overload arity,
extension receivers, properties, classes, and actual type aliases). Default presence is published
on the surviving actual declaration while a temporary expect-provider identity remains inside
Pass 1. Its expression is checked immediately into FIR owned by the actual callable and retained in
`DefaultArgumentStore`; neither provider syntax nor a provider coordinate crosses into Pass 2. The selected expect declaration subtree is also removed from the
compact inventory before it can contribute
signature lookup input. Explicit declaration-type candidate discovery now walks only compact header
type roots. Top-level callable and property publication also reads receivers, parameters/context
parameters, explicit result/property types, type parameters, and bounds from the compact arena; the
ordinary resolver remains the single owner of classifier selection and semantic `Ty` construction.
File-scope type-alias identities, formals, and expansion targets now come from compact headers as
well. Classifier publication now also reads declaration type parameters, bounds and variance,
primary-constructor parameter types and property flags, and superclass/interface types and type
arguments from the compact arena. Explicit member callable/property signatures and secondary-
constructor parameter types now use their owner-qualified compact declarations as well. Annotations,
locals, body-only type uses, nested aliases, and inferred-expression publication temporarily remain
on legacy declaration paths. The complete compact inventory is explicitly dropped before ordinary
body checking. Inferred-expression publication and body checking still retain `File`/`TypeInfo`, so
these are connected migration seams, not the completed two-pass cutover.

For valid source sets, production now projects the completed semantic table through the stable
Pass-1 identities into a pending/error-free `ResolvedModuleIndex`, then consumes the compact header
module with `StreamedHeaderModule::finish`. That operation drops lookup spellings, scopes, and
header type syntax and yields the stable `SourceMap` plus a Pass-1-only executable inventory.
That inventory is partitioned from resolved callable flags, used to check inline units and
signature-owned parameter defaults, and destroyed before Pass 2. Their FIR is installed in
`InlineBodyStore` and `DefaultArgumentStore`. Ordinary bodies are discovered only from the live
sequential Pass-2 parse. The old
unresolved-return fallback has also been removed: a recursive
inferred function cycle receives the Kotlin-compatible diagnostic for every declaration on the
cycle, and any remaining private `Ty::Pending` collection marker is retired as a failed signature,
never as `Unit`. Invalid inferred signatures still rely on parts of the legacy checker for their
non-cycle diagnostics, so the stable index is currently present only when every signature publishes
successfully.

Production installs that index, source map, prepared inline FIR, and checked default FIR in the
persistent `FrontendModule` container. Defaults are retained signature payload keyed by stable
callable identity, not an ordinary-body locator; they are consumed when their owning source reaches
common lowering. No ordinary-body inventory or parser-unit coordinate crosses the pass boundary. The current
migration seam checks only the declaration roots and exact body units required by inline functions
and inline accessors; neighboring ordinary bodies are excluded, and anonymous-object capture
preparation uses the same selection. Ordinary captures are discovered from the active reparsed
Pass-2 file. Production inline checking now uses only the selected compacted fragment from the
initial Pass-1 parse, creates and consumes that source's `TypeInfo`, moves its checked FIR into
`InlineBodyStore`, and destroys the complete legacy `File` immediately. Sources with no inline body
are destroyed before inline checking starts; an enforced boundary assertion rejects any surviving
declaration, expression, or statement arena. Pass 2 reparses source and streams ordinary checked
bodies.

The production signature evaluator itself no longer retains `File` or consults parser arenas.
Package/alias/star imports and the explicit-context language mode come from the packed file scope;
lexical classifier containment comes from stable declaration ranges; classifier, callable,
property, constructor, and member-extension dependencies use stable semantic identities. Source
class signatures now carry their stable `DeclarationId`, and interface delegation is packed as
resolved supertype/constructor-parameter ordinals. Enum-entry member signatures are projected
directly from compact declaration headers, including their declaration-owned generic parameter
scope; no enum-entry AST adapter or side map remains. The finalizer and demand-driven solver are
structurally incapable of receiving `File` or parser-arena identities.

After solving, a stable-ID-only migration adapter projects finalized results into the legacy checker
view still used by Pass 2. It performs no lookup or inference. Generic top-level and member-extension
properties now keep one declaration-owned type-parameter identity across compact inference, Pass-2
checking, common IR, accessor generic signatures, and class metadata; declared source names are
retained only as metadata payload.

The production publication adapter routes every inferred stub through direct AST-to-`SigExpr`
extraction and resolver-backed `SignatureGraph` evaluation; explicit stubs use `publish_explicit`.
Solver finalization consumes and drops the graph before the index is accepted. There is no legacy
`Known` backfill for an unextracted inferred declaration: extraction or evaluation failure blocks
the streamed index. Current compact nodes cover literals, names, calls/invokes, members, operators,
indexing, branches/joins, casts/nullability, typed locals/destructuring, delegated properties and
locals, ordinary local functions, typed/receiver/suspend anonymous functions, and unambiguous
top-level callable references. Call nodes retain packed named/spread argument facts, trailing-lambda
placement, and explicit type arguments without parser IDs. Positional calls and full-permutation
named calls forward explicit types and spread kinds through the ordinary resolver. Named calls with
omitted defaults use the shared argument mapper and an explicit `OmittedDefault` resolver argument,
so absent expressions contribute no generic constraint. Candidate families whose overloads imply
different named-argument slot layouts still need the resolver's fully candidate-aware mapped-call
seam. Positional postponed lambdas use compact parameter placeholders: the ordinary resolver first
supplies the selected function expectation, the graph checks the body with those exact inputs,
and final overload/generic selection consumes the materialized function type. This covers implicit
`it`, explicitly named untyped parameters, top-level HOFs, member HOFs, and stdlib extensions such as
`List.map`. The shared resolver can normalize a selected SAM parameter to a function expectation,
but pre-selection among untyped-lambda SAM candidates is still checker-only. Receiver/context
lambdas, lambda-return-selected overload families, expected-type-dependent references, and the
remaining local declaration forms still need the corresponding normal-resolver seams. Named
postponed lambdas now share the ordinary named/default slot mapper for both top-level and member
calls. Recursive-inference diagnostics are graph-owned and emitted at stable inferred-body ranges;
other failed selections still need exact graph-owned diagnostic records.

Body checking now has a real, independently sized `body_check` module. During Pass 1, inline syntax
flows through AST-to-FIR checking and the resulting body moves into `InlineBodyStore`. During Pass 2,
an ordinary `BodyWorkItem` identifies the stable declaration in reparsed source and its checked FIR
moves by value to `CheckedBodySink`; focused tests exercise the current migration seam. The checker
currently constructs pending-free FIR for constants, annotation arrays, lexical locals and writes,
blocks, templates, null assertions, Elvis, casts/tests, conditionals, `when`, `try`, throws, and
checker-confirmed builtin unary/binary/range operations. Same-module top-level, explicit-member, and
implicit-member calls carry stable `DeclarationId` through the ordinary candidate model and become
`CallableId` in FIR, including cross-file calls, argument/default mapping, generic substitutions, and
selected custom operator conventions. Their checked argument records retain source evaluation order
for reordered named arguments, carry omitted defaults explicitly, and preserve vararg element/spread,
empty-pack, and named whole-array decisions without parser IDs. Checked loop FIR owns stable
break/continue targets and explicit range/array/String iteration headers; counter and builtin
iterable families are closed enums, so lowering has no unsupported loop fallback. Indexed writes
are structurally array-only, and destructuring entries are either ignored or fully bound rather than
three independently optional fields. Indexed reads/writes, numeric inc/dec, and source property
reads/writes are explicit FIR operations. Properties retain a stable `PropertyId`; member-extension
properties retain independently selected dispatch and extension receivers, while bare member access
retains the selected receiver-tower coordinate instead of a source label or type search. Safe member
calls, reads, and writes wrap an already-selected selector, so the null path does not evaluate a
write value or repeat lookup. Function-value/operator invocation, stable
top-level/member callable references, in-place compound-assignment calls, positional destructuring,
ordinary/receiver lambdas, and ordinary local functions now have checked FIR. Lambda/local-function
captures retain body-local value paths, final types, and shared-cell decisions; recursive local calls
use enclosing-body callable coordinates rather than parser statement IDs. Dependency property
identities, delegates, synthetic constructors, local classes/type
aliases, classifier/dependency callable references, and several conversion/data-flow decisions remain
explicit migration failures. Source callable references now materialize common-IR lambda adapters for
static, bound, unbound, extension, generic, default-adapted, and vararg-adapted module targets,
including cross-file targets. Ordinary and default-adapted local references also materialize lambda
adapters over lifted callable identities; bound and unbound local extension references preserve the
selected receiver binding, including default/vararg adaptation and suspend conversion. Source
and dependency function references use module/provider-owned stable identities respectively;
property references retain stable identity,
binding, receiver placement, and mutability. Implicit context arguments are mapped directly onto their
declaration parameter ordinals for source and local calls. Stable property initializer, delegate,
getter, setter, class/enum-entry init-block, and script body units now route independently through
the consuming checker. Enum-entry construction FIR also owns the selected primary constructor and
the shared argument mapper's final source-order named/default/vararg decisions. Source and dependency
constructor calls now use separate stable/backend-neutral target variants; dependency constructors
retain the selected classifier and semantic parameter signature without a fabricated module ID.
Primary and secondary constructor body units now own checked parameter defaults and an explicit
`ConstructorDelegation` FIR statement. The resolver's single constructor selector supplies final
source-order slots, omitted defaults, and vararg decisions for `this(...)` and `super(...)` alike.
Same-module superclass targets carry their stable constructor declaration; dependency targets carry
the selected classifier and semantic parameter signature. Compiler-supplied enum name/ordinal
forwarding has no source delegation node and remains a backend representation responsibility.
Callable body units also own checked parameter-default FIR, including abstract/body-less declarations;
defaults remain excluded from signature constraints. Production emission now reparses one active
file, constructs checked FIR, consumes it into common IR, and drops that file's AST, `TypeInfo`, FIR,
and IR before advancing. The emission-oriented Pass-1 API also drops its whole-module AST before
returning and no longer accumulates a module-sized `TypeInfo` vector during inline preparation.
Pass-1 peak memory is not conformant yet: signature collection still temporarily builds a
whole-module legacy `File`/symbol view, and Pass 2 still carries the legacy `SymbolTable` to recreate
resolver state. Module type-alias lookup and declaration-spelling metadata have moved to the
finalized index, and their duplicate spelling/parser-coordinate maps are now destroyed before Pass
2. Annotation occurrence bindings keyed by source ranges are also destroyed: stable declaration
annotations live in the index, while body-local applications resolve in the active lexical unit.
Classifier, callable, and body-local publication queries remain on the table. Those seams must be
replaced by the compact headers/index rather than hidden behind lifetime assertions.

The target has exactly two source passes:

1. Pass 1 extracts headers and compact inferred-signature expressions, solves every non-local
   signature, checks syntax retained only for semantically inline declarations and parameter
   defaults, stores that checked FIR as signature payload, and destroys the signature graph and
   temporary syntax.
2. Pass 2 reparses source and streams each ordinary body through checked FIR, common IR, and the
   backend, dropping all transient syntax/FIR/IR after the consuming callback.

There is no separate inline source pass and no retained body-text locator.

## Declarations and body forms

| Family | Parser forms | Current representation | Streaming FIR |
| --- | --- | --- | --- |
| Top-level/nested declarations | `Fun`, `Class`, `Property` | Stable headers/index; active Pass-2 AST only | Checked FIR streamed; declaration metadata incomplete |
| Callable bodies | absent body, expression body, block body | Active body unit | Checked FIR streamed; remaining operation families listed below |
| Constructors | primary, secondary, `this`, `super`, implicit delegation | Stable constructor/body identities | Delegation materialized; default/outer edge cases pending |
| Property accessors | expression/block getter, default/body setter | Stable property/accessor identities; inline accessor FIR retained in Pass 1 | Storage/accessors materialized; delegated/context/extension edges pending |
| Initialization | property initializer, `init` block, enum-entry initializer | Stable ordered body units | Checked FIR streamed and source order materialized |
| Scripts | source-ordered script block | Stable script body unit | Checked FIR exists; backend script emission separately unsupported |

## Expressions

| Parser forms | Current representation | Streaming FIR |
| --- | --- | --- |
| `IntLit`, `LongLit`, `UIntLit`, `ULongLit`, `DoubleLit`, `FloatLit`, `BoolLit`, `StringLit`, `CharLit`, `NullLit` | Active AST | FIR → common IR complete |
| `AnnotationArrayLiteral` | Active AST + resolved elements | Checked FIR complete; metadata realization audit pending |
| `UnsupportedAnnotationArgument` | parser-only semantic diagnostic | Parser-only diagnostic |
| `Name`, `Member`, `ExtensionAccess`, `Index` | Active AST + selected identities | Checked FIR; delegated/context properties and some conventions pending |
| `Call`, `SafeCall`, `CallableRef` | Active AST + final call mapping | Ordinary calls plus source and dependency function references streamed, including default/vararg/suspend adaptation; property/classifier and reified-inline edges pending |
| `NotNull`, `Elvis`, `Unary`, `Binary`, `IncDec` | Active AST + selected operators | Builtins/source operators complete; unsigned convention realization pending |
| `If`, `Block`, `When`, `Try` | Active AST + final data-flow types | FIR → common IR complete for supported checker paths |
| `Throw`, `Return`, `Break`, `Continue` | Active AST + stable targets | FIR → common IR complete |
| `Lambda` | Active AST + checked captures | Ordinary/capturing/SAM closures materialized; reified/non-local inline splicing pending |
| `Is`, `As`, `InRange`, `RangeTo` | Active AST + resolved operands | Type operations complete; unsigned range value/contains realization pending |
| `Template` | Active AST + final part types | FIR → common IR complete |

## Statements

| Parser forms | Current representation | Streaming FIR |
| --- | --- | --- |
| `Local`, `LocalLateinit`, `LocalDelegate`, `Destructure` | Active AST + checked bindings | Ordinary locals/destructure complete; delegates pending |
| `Assign`, `AssignMember`, `AssignIndex`, `CompoundAssign`, `IncDec` | Active AST + selected write/operator | FIR present; property/index convention edges pending |
| `Return`, `Break`, `Continue` | Active AST + stable control target | FIR → common IR complete |
| `While`, `DoWhile`, `For`, `ForEach` | Active AST + checked loop protocol | While/counting/source/dependency-iterator loops complete; unsigned loops pending |
| `Expr` | Active AST + final type | Complete when its expression family is complete |
| `LocalFun`, `LocalClass`, `LocalTypeAlias` | Active AST + stable local identities | Local functions and local classifiers, including capture fields, are materialized; generic local aliases and remaining local-class convention edges are pending |

## Type syntax

`TypeRef` is a packed struct rather than an enum. The exhaustive compact header copy includes
classifier and qualified references, type arguments, nullable types, definitely-non-null types,
function types (receiver, context and suspend forms), `in`/`out` projections, star projections,
type-parameter references, type aliases, and declaration-site bounds. Production import/classifier
candidate discovery now consumes this compact copy for declaration headers, but final `Ty`
publication has moved for top-level callables, properties, file-scope type aliases, classifier
type parameters/supertypes, constructors, and explicit member callables/properties. Nested aliases
and inferred member/property expressions still read legacy AST nodes and sparse AST-keyed tables.
The next cutover must extend the same ordinary-resolver adapter to those remaining declarations.
The FIR path must embed the final qualified classifier identity and a pending-free semantic type at
every use.

## Migration gates

1. The coverage classifiers remain exhaustive and wildcard-free.
2. A row moves to FIR-complete only with a focused repository regression and kotlinc comparison
   where behavior or diagnostics differ.
3. `ir_lower` AST/`TypeInfo` reads are removed with each migrated family; compatibility branches
   are not retained.
4. The temporary `SignatureGraph` is allowed only before signature finalization and is never a
   field of the persistent frontend module.
