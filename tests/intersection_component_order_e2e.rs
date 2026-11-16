//! An inferred intersection lists its components in the order kotlinc's common supertype finds
//! them: the order of the first operand's supertypes. The same set built from operands in another
//! order is the same type, spelled in that other order.

use super::common;

#[test]
fn a_joined_intersection_is_spelled_in_its_first_operands_supertype_order() {
    let source = "\
interface Zed
interface Alpha
interface Mid
class X : Zed, Mid, Alpha
class Y : Mid, Alpha, Zed
val s: String = listOf(X(), Y())
val t: String = listOf(Y(), X())
fun same(flag: Boolean) {
    val first = listOf(X(), Y())
    val second = listOf(Y(), X())
    val both: List<Zed> = if (flag) first else second
}
fun joined(flag: Boolean): Zed {
    val first = listOf(X(), Y()).first()
    val second = listOf(Y(), X()).first()
    val either = if (flag) first else second
    return either
}
";
    let sources = [("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:6:15: initializer type mismatch: expected 'String', actual 'List<Zed & Mid & Alpha>'.",
            "test.kt:7:15: initializer type mismatch: expected 'String', actual 'List<Mid & Alpha & Zed>'.",
        ]
    );
}
