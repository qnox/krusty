//! A substituted scalar passed to a declaration's erased generic parameter is boxed.
//!
//! A call site sees a generic declaration's parameters substituted (`T` = `Int`), while the
//! declaration is compiled once against its erased types. Every boundary that is not an ordinary
//! selected call must still box the scalar and unbox the erased result, as kotlinc does: a local
//! function call, a superclass constructor delegation from a primary or secondary constructor, a
//! specialized property reference's adapter, a property accessor's context argument, and a
//! for-loop's `iterator()` convention call on a generic extension receiver. krusty passed the
//! scalar unchanged and failed verification.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "open class Base<T>(val value: T)\n\
class FromPrimary : Base<Long>(-1L)\n\
class FromSecondary : Base<Int> {\n\
    constructor(seed: Int) : super(seed + 1)\n\
}\n\
fun localIdentity(n: Int): Int {\n\
    fun <T> same(value: T): T = value\n\
    return same(n) + 1\n\
}\n\
val <T> T.itself: T get() = this\n\
fun itselfReference(): (Int) -> Int = Int::itself\n\
class Scaled<Y> {\n\
    context(factor: Y)\n\
    val scale: Y get() = factor\n\
}\n\
fun Int.contextual(scaled: Scaled<Int>): Int = scaled.scale\n\
interface Steps<T> {\n\
    operator fun hasNext(): Boolean\n\
    operator fun next(): T\n\
}\n\
operator fun <T : Any> T?.iterator(): Steps<T> = object : Steps<T> {\n\
    private var pending = this@iterator != null\n\
    override fun hasNext(): Boolean = pending\n\
    override fun next(): T { pending = false; return this@iterator!! }\n\
}\n\
fun loopOnce(n: Int): Int {\n\
    var total = 0\n\
    for (step in n) total += step\n\
    return total\n\
}\n";

#[test]
fn scalar_arguments_reach_erased_parameters() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (FromPrimary().value != -1L) return \"fail primary\"\n\
             \x20   if (FromSecondary(16).value != 17) return \"fail secondary\"\n\
             \x20   if (localIdentity(9) != 10) return \"fail local\"\n\
             \x20   if (itselfReference()(4) != 4) return \"fail reference\"\n\
             \x20   if (5.contextual(Scaled<Int>()) != 5) return \"fail context\"\n\
             \x20   if (loopOnce(3) != 3) return \"fail loop\"\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "ErasedParameters",
    );
}

#[test]
fn scalar_arguments_are_boxed_where_kotlinc_boxes_them() {
    for (class, members) in [
        (
            "ErasedParametersKt",
            &["int localIdentity(", "int contextual(", "int loopOnce("][..],
        ),
        ("FromPrimary", &["FromPrimary()"][..]),
        ("FromSecondary", &["FromSecondary(int)"][..]),
    ] {
        let built = compare_with_kotlinc_plugin(
            "ErasedParameters",
            SOURCE,
            class,
            &[common::stdlib_jar()],
            "25",
            &[],
        )
        .expect("reference kotlinc is provisioned");
        for member in members {
            let reference = method_instructions(&built.reference, member);
            assert!(!reference.is_empty(), "kotlinc: {class} {member} not found");
            assert_eq!(
                method_instructions(&built.krusty, member),
                reference,
                "{class} {member}"
            );
        }
    }
}
