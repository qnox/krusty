//! Regression pins for real Kotlin codegen/box-corpus cases — driven through krusty + the JVM via the
//! SAME corpus the differential conformance gate runs over (`common::run_box_corpus_case`), instead of
//! hand-written snippets that keep hitting lowering edges the corpus cases don't. These are the exact
//! cases that the `Ty::Nullable` rework miscompiled (a 57-case `VerifyError` the hand-written e2e
//! missed entirely) — all nullable-primitive boxing / boxed-comparison shapes — now `box()=OK`.
//!
//! Needs the provisioned box corpus (`KRUSTY_KOTLIN_BOX_DIR`) + JVM toolchain; skips otherwise.

mod common;

/// Single-file `// WITH_STDLIB` box cases from the corpus that regressed under the nullable rework and
/// must stay `box()=OK`. (Multi-file `// FILE:` cases and ones needing features krusty skips are left
/// to the full conformance gate.)
const REGRESSED_CASES: &[&str] = &[
    "binaryOp/eqNullableShortToShort.kt",
    "binaryOp/eqNullableToPrimitiveWithSideEffects.kt",
    "boxing/boxing10.kt",
    "boxingOptimization/kt5588.kt",
    "casts/genericReturnCast.kt",
    "constants/constantsInWhen.kt",
    "controlStructures/compareBoxedIntegerToZero.kt",
    "dataClasses/hashCode/null.kt",
    "ieee754/nullableIntEquals.kt",
    "inline/requireNotNull.kt",
];

/// A broader cross-subsystem net of real corpus cases krusty box-OKs — touching the areas the Ty/IR
/// consolidation reworked (a data-class multi-declaration / IrField, a smart-cast, a generic-constant
/// `as`, a `when`, control flow). Curated to box-OK today (the conformance gate proves the full set);
/// pinned here for fast, attributable feedback on a regression.
const SUBSYSTEM_CASES: &[&str] = &[
    "dataClasses/multiDeclarationFor.kt",
    "smartCasts/smartCastInsideIf.kt",
    "casts/asForConstants.kt",
    "when/noElseExhaustiveStatement.kt",
    "controlStructures/kt769.kt",
];

#[test]
fn subsystem_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(SUBSYSTEM_CASES);
}

/// `removeLastOrNull() ?: throw ...` inside `buildList` exposes nullable generic extension returns:
/// Elvis with a diverging RHS must produce the non-null LHS type, not keep `Any?` and lose conformance.
const ELVIS_NOTHING_GENERIC_CASES: &[&str] = &["inference/pcla/issues/kt49887.kt"];

#[test]
fn elvis_nothing_generic_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(ELVIS_NOTHING_GENERIC_CASES);
}

/// Representative cases from the primitive-member regression: source-level primitive methods lower from
/// Kotlin semantics, not as virtual calls on raw primitive stack values.
const PRIMITIVE_MEMBER_CASES: &[&str] = &[
    "binaryOp/primitiveEqualsSafeCall.kt",
    "boxingOptimization/explicitEqualsOnDouble.kt",
    "intrinsics/nonShortCircuitAnd.kt",
];

#[test]
fn primitive_member_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(PRIMITIVE_MEMBER_CASES);
}

/// Narrow primitive increments/decrements must wrap at the source type width in JVM emission.
const PRIMITIVE_INC_DEC_CASES: &[&str] = &["intrinsics/kt12125.kt", "intrinsics/kt12125_inc.kt"];

#[test]
fn primitive_inc_dec_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(PRIMITIVE_INC_DEC_CASES);
}

/// Unsigned value-class `compareTo` is a JVM backend realization, not a virtual call on raw int/long.
const UNSIGNED_COMPARE_CASES: &[&str] = &[
    "unsignedTypes/unsignedIntCompare.kt",
    "unsignedTypes/unsignedLongCompare.kt",
];

#[test]
fn unsigned_compare_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(UNSIGNED_COMPARE_CASES);
}

/// Nullable primitive wrappers use Kotlin's null-safe `Any?.hashCode()` behavior.
const NULLABLE_HASH_CODE_CASES: &[&str] = &["primitiveTypes/booleanHashCode.kt"];

#[test]
fn nullable_hash_code_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(NULLABLE_HASH_CODE_CASES);
}

/// Source `get(index)` members on JVM string-like receivers dispatch to `charAt(index)`.
const STRING_GET_CASES: &[&str] = &[
    "extensionFunctions/simple.kt",
    "primitiveTypes/kt4098.kt",
    "primitiveTypes/kt4210.kt",
    "strings/kt5389_stringBuilderGet.kt",
];

#[test]
fn string_get_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(STRING_GET_CASES);
}

/// Source `String.plus(Any?)` has no JVM virtual method; the JVM backend realizes it as concatenation.
const STRING_PLUS_CASES: &[&str] = &[
    "classes/kt1759.kt",
    "strings/simpleStringPlus.kt",
    "strings/twoArgumentNullableStringPlus.kt",
];

#[test]
fn string_plus_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(STRING_PLUS_CASES);
}

/// Source primitive `rangeTo` members have no JVM virtual method; the JVM backend realizes them as
/// range-class construction.
const RANGE_TO_CASES: &[&str] = &[
    "controlStructures/kt2291.kt",
    "intrinsics/longRangeWithExplicitDot.kt",
    "primitiveTypes/rangeTo.kt",
];

#[test]
fn range_to_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(RANGE_TO_CASES);
}

/// Small-integer range/progression cases exercise source-level `Byte`/`Short` extension resolution
/// without pushing JVM physical descriptor details into the common resolver.
const RANGE_SMALL_INT_CASES: &[&str] = &[
    "ranges/contains/intInByteRangeWithPossibleOverflow.kt",
    "ranges/contains/intInShortRangeWithPossibleOverflow.kt",
    "ranges/expression/inexactDownToMinValue.kt",
    "ranges/expression/overflowZeroToMinValue.kt",
    "ranges/expression/progressionDownToMinValue.kt",
    "ranges/literal/inexactDownToMinValue.kt",
    "ranges/literal/progressionDownToMinValue.kt",
];

#[test]
fn range_small_int_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(RANGE_SMALL_INT_CASES);
}

/// `ClosedRange<T>.contains(T)` resolves as a metadata inline extension and must splice, while adjacent
/// public `kotlin.test.assertTrue` calls must remain calls to the public facade, not optional-spliced
/// package-part bridges.
const RANGE_CONTAINS_CASES: &[&str] = &[
    "ranges/contains/inOptimizableIntRange.kt",
    "ranges/contains/inOptimizableLongRange.kt",
];

#[test]
fn range_contains_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(RANGE_CONTAINS_CASES);
}

/// Unsigned exclusive ranges (`until` / `..<`) are provider-owned range helpers (`URangesKt`) plus the
/// ordinary generic range lowering path.
const UNSIGNED_UNTIL_RANGE_CASES: &[&str] = &[
    "ranges/contains/generated/uintRangeUntil.kt",
    "ranges/stepped/unsigned/literal/until/stepOne.kt",
    "ranges/stepped/unsigned/expression/rangeUntil/stepOne.kt",
];

#[test]
fn unsigned_until_range_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(UNSIGNED_UNTIL_RANGE_CASES);
}

/// `kotlin.test.assertFailsWith<T> { ... }` is a private defaulted reified inline helper in kotlin-test.
/// The resolver must see the non-public `$default` candidate, and lowering must realize the reified
/// exception check without calling a package-private implementation class.
const ASSERT_FAILS_WITH_CASES: &[&str] = &[
    "ranges/stepped/expression/rangeTo/illegalStepZero.kt",
    "ranges/stepped/expression/until/illegalStepNegative.kt",
    "ranges/stepped/unsigned/expression/rangeTo/illegalStepZero.kt",
];

#[test]
fn assert_fails_with_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(ASSERT_FAILS_WITH_CASES);
}

/// Kotlin collection mapped properties whose JVM names differ.
const COLLECTION_MAPPED_CASES: &[&str] = &["operatorConventions/plusAssignWithComplexRHS.kt"];

#[test]
fn collection_mapped_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(COLLECTION_MAPPED_CASES);
}

/// Reflection property names are source properties; the JVM runtime exposes them as getters.
const REFLECTION_PROPERTY_CASES: &[&str] = &[
    "callableReference/property/simpleMember.kt",
    "callableReference/property/simpleTopLevel.kt",
    "inlineClasses/functionNameMangling/reflectionForPropertyOfInlineClassType.kt",
    "inlineClasses/functionNameMangling/reflectionForPropertyOfInlineClassTypeGeneric.kt",
];

#[test]
fn reflection_property_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(REFLECTION_PROPERTY_CASES);
}

/// `Result<T>` is an inline/value class whose public API compiles to static `*-impl` members. These
/// cases pin inferred expression-body returns that flow through `Result.getOrNull()` and bridge dedupe
/// after value-class erasure. The object-literal case also pins bridge parameter erasure: external
/// value classes such as `Result` are already carried as their underlying `Object`, not as boxed
/// instances that can be `checkcast` + `unbox-impl`ed.
const RESULT_VALUE_CLASS_CASES: &[&str] = &[
    "inlineClasses/result/directCall2.kt",
    "inlineClasses/result/inlineMethodOnResult.kt",
    "inlineClasses/unboxGenericParameter/objectLiteral/resultAny.kt",
];

#[test]
fn result_value_class_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(RESULT_VALUE_CLASS_CASES);
}

/// Interface property/function returns declared as nullable value classes must stay boxed even when an
/// implementation overrides them with `Nothing`/`Nothing?`; otherwise the call descriptor returns the
/// primitive underlying while the nullable call site emits reference operations.
const VALUE_CLASS_NOTHING_OVERRIDE_CASES: &[&str] = &["inlineClasses/overrideReturnNothing.kt"];

#[test]
fn value_class_nothing_override_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(VALUE_CLASS_NOTHING_OVERRIDE_CASES);
}

/// Generic value classes are modeled by the JVM value-class pass with an erased `Object` underlying
/// representation plus nullability from type-parameter bounds. The emitter must not reject them
/// wholesale after lowering; unsupported sub-shapes should fail at their precise boundary instead.
const GENERIC_VALUE_CLASS_CASES: &[&str] = &[
    "inlineClasses/genericUnderlyingValue/simple.kt",
    "inlineClasses/genericUnderlyingValue/simple2.kt",
    "inlineClasses/genericUnderlyingValue/upperBound.kt",
    "inlineClasses/toStringCallingPrivateFunGeneric.kt",
    "inlineClasses/unboxGenericParameter/objectLiteral/stringGeneric.kt",
];

#[test]
fn generic_value_class_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(GENERIC_VALUE_CLASS_CASES);
}

/// Inner-class constructors prepend the captured outer instance before source constructor parameters;
/// `super(...)` argument lowering must account for that synthetic slot.
const INNER_CONSTRUCTOR_CASES: &[&str] = &["classes/inner/properSuperLinking.kt"];

#[test]
fn inner_constructor_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(INNER_CONSTRUCTOR_CASES);
}

/// An external value class (`Result`) returned through a GENERIC / `Any` supertype boundary must be
/// BOXED at that boundary (`Result.box-impl`, so the caller observes the boxed object's `toString`),
/// and UNBOXED again at a consuming `as Result` cast (`checkcast kotlin/Result; unbox-impl`, so a
/// following `getOrThrow()` / `==` sees the underlying). The override method's own direct call stays
/// unboxed. These single-file cases pin BOTH sides of that ABI (box at the bridge, unbox at the cast);
/// the multi-MODULE variant (`inlineClasses/result/returnGenericMultiModule.kt`) exercises the same
/// path across a separately-compiled classpath boundary and is covered by the full conformance gate.
const VALUE_CLASS_GENERIC_BOUNDARY_CASES: &[&str] = &[
    "inlineClasses/returnResult/classAnyOverride.kt",
    "inlineClasses/returnResult/classGenericOverride.kt",
    "inlineClasses/returnResult/classResultOverride.kt",
];

#[test]
fn value_class_generic_boundary_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(VALUE_CLASS_GENERIC_BOUNDARY_CASES);
}

/// Run each corpus case; a case that RAN must be `box()=OK` (any other value is a real miscompile),
/// while `None` (skipped: declined / multi-file) is tolerated exactly as the conformance gate counts
/// it — so this e2e is never stricter than the gate. Guards against the whole set silently all-skipping.
fn assert_corpus_cases_box_ok(cases: &[&str]) {
    if !common::corpus_ready() {
        return;
    }
    let mut failures = Vec::new();
    let mut ran = 0;
    for &rel in cases {
        if let Some(out) = common::run_box_corpus_case(rel) {
            ran += 1;
            if out != "OK" {
                failures.push(format!("{rel}: {out}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} corpus case(s) miscompiled (not box()=OK):\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        ran > 0,
        "no corpus case ran with the corpus provisioned — classpath/provisioning broken?"
    );
}

#[test]
fn nullable_boxing_corpus_cases_box_ok() {
    assert_corpus_cases_box_ok(REGRESSED_CASES);
}
