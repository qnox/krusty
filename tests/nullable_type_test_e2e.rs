//! kotlinc's `TypeOperatorLowering.lowerInstanceOf` shapes a type test from the operand's type as
//! well as the target's. A nullable operand tested against a nullable target is bound once and
//! becomes `tmp == null || tmp is T`, a plain `when` whose stored value the optimizer then keeps on
//! the stack (`dup; ifnonnull; pop; iconst_1`). A nullable operand tested against a non-null target
//! with the same erased upper bound (`x is T` for `x: T?`) is the null check `x != null`. A
//! non-null operand tests the non-null target. A reified type parameter is tested as written, and
//! its marker names `T?` when the target is nullable. krusty expanded every nullable target to an
//! eager `ior` of a stored null test, tested a same-bound target with `instanceof`, and named a
//! nullable reified target `T`.

use super::common;

const DECLARATIONS: &str = "open class Base\n\
class Item : Base()\n\
\n\
fun make(): Any? = null\n\
\n\
fun nullableTarget(b: Any?) = b is Item?\n\
fun notNullableTarget(b: Any?) = b !is Item?\n\
fun nullableCondition(b: Any?): Int { if (b is Item?) return 1; return 2 }\n\
fun notNullableCondition(b: Any?): Int { if (b !is Item?) return 1; return 2 }\n\
fun nullableCallOperand() = make() is Item?\n\
fun nullableNarrowed(b: Base?) = b is Item?\n\
fun nullableBranch(b: Any?) = when (b) { is Item? -> 1; else -> 2 }\n\
fun nullableAnd(b: Any?, flag: Boolean) = b is Item? && flag\n\
fun nonNullOperand(b: Base) = b is Item?\n\
fun <T> parameterOperand(x: T) = x is Item?\n\
\n\
fun sameClass(x: Item?) = x is Item\n\
fun notSameClass(x: Item?) = x !is Item\n\
fun <T : Base> sameBound(x: T?) = x is T\n\
fun <T : Base> notSameBound(x: T?) = x !is T\n\
fun <T : Base> sameBoundCondition(x: T?): Int { if (x is T) return 1; return 2 }\n\
fun <T : Base> notSameBoundCondition(x: T?): Int { if (x !is T) return 1; return 2 }\n\
fun <T> anyBound(x: T) = x is Any\n\
fun <T> otherBound(x: T) = x is Item\n\
fun subclass(x: Base?) = x is Item\n\
\n\
inline fun <reified T> isT(x: Any?) = x is T\n\
inline fun <reified T> isNullableT(x: Any?) = x is T?\n\
inline fun <reified T> isNotNullableT(x: Any?) = x !is T?\n\
inline fun <reified T> asNullableT(x: Any?) = x as T?\n";

/// Call sites of the reified declarations, which specialize each marker at the call.
const REIFIED_CALLS: &str = "fun reifiedNullable(b: Any?) = isT<Item?>(b)\n\
fun reifiedNullableTarget(b: Any?) = isNullableT<Item>(b)\n\
fun reifiedNotNullableTarget(b: Any?) = isNotNullableT<Item>(b)\n\
fun reifiedNullableCast(b: Any?) = asNullableT<Item>(b)\n";

#[test]
fn a_type_test_takes_kotlincs_shape_for_its_operand_and_target() {
    common::assert_classes_identical_to_kotlinc(
        "NullableTypeTests",
        DECLARATIONS,
        &["Base", "Item", "NullableTypeTestsKt"],
    );
}

/// The specialized call sites, instruction for instruction. Their classes are not compared whole:
/// krusty writes no SMAP for a same-file inline call yet (a documented residual), so the facade's
/// `SourceDebugExtension` and the inlined line numbers differ for reasons unrelated to type tests.
#[test]
fn a_reified_nullable_type_test_specializes_like_kotlinc() {
    let source = format!("{DECLARATIONS}{REIFIED_CALLS}");
    let built = common::compare_with_kotlinc_plugin(
        "NullableTypeTestCalls",
        &source,
        "NullableTypeTestCallsKt",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    for member in [
        " isNullableT(",
        " isNotNullableT(",
        " asNullableT(",
        " reifiedNullable(",
        " reifiedNullableTarget(",
        " reifiedNotNullableTarget(",
        " reifiedNullableCast(",
    ] {
        let reference = common::method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "kotlinc: {member} not found");
        assert_eq!(
            common::method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

#[test]
fn a_type_test_answers_null_and_instances_like_kotlinc() {
    let source = format!(
        "{DECLARATIONS}{REIFIED_CALLS}\
fun answers(v: Any?): String =\n\
    \"${{nullableTarget(v)}}${{notNullableTarget(v)}}${{nullableCondition(v)}}${{notNullableCondition(v)}}\" +\n\
    \"${{nullableBranch(v)}}${{nullableAnd(v, true)}}${{parameterOperand(v)}}${{anyBound(v)}}${{otherBound(v)}}\" +\n\
    \"${{reifiedNullable(v)}}${{reifiedNullableTarget(v)}}${{reifiedNotNullableTarget(v)}}\"\n\
fun box(): String {{\n\
    val item = Item()\n\
    val base = Base()\n\
    val r = answers(null) + \"|\" + answers(item) + \"|\" + answers(base) + \"|\" + answers(\"s\") + \"|\" +\n\
        \"${{nullableCallOperand()}}${{nullableNarrowed(null)}}${{nullableNarrowed(base)}}${{nonNullOperand(item)}}\" +\n\
        \"${{nonNullOperand(base)}}${{sameClass(null)}}${{sameClass(item)}}${{notSameClass(null)}}\" +\n\
        \"${{sameBound<Base>(null)}}${{sameBound(base)}}${{notSameBound<Item>(null)}}${{sameBoundCondition(item)}}\" +\n\
        \"${{notSameBoundCondition<Base>(null)}}${{subclass(item)}}${{subclass(base)}}${{subclass(null)}}\" +\n\
        \"${{reifiedNullableCast(null)}}${{reifiedNullableCast(item) === item}}\"\n\
    return if (r == EXPECTED) \"OK\" else r\n\
}}\n"
    );
    common::expect_box_same_as_kotlinc(
        &source.replace("EXPECTED", &format!("\"{EXPECTED}\"")),
        "NullableTypeTestsRun",
    );
}

/// kotlinc's answers for `null`, an `Item`, a `Base` and a `String`, then the single checks.
const EXPECTED: &str = "truefalse121truetruefalsefalsetruetruefalse|\
truefalse121truetruetruetruetruetruefalse|\
falsetrue212falsefalsetruefalsefalsefalsetrue|\
falsetrue212falsefalsetruefalsefalsefalsetrue|\
truetruefalsetruefalsefalsetruetruefalsetruetrue11truefalsefalsenulltrue";
