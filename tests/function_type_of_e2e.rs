//! A function type's `typeOf` and its `FunctionN` spelling name one `KType`. On Native both are the
//! one program-wide static object, defined once even when one file names both spellings.

use super::common;

#[test]
fn a_function_type_and_its_function_n_spelling_are_one_type_on_jvm_and_native() {
    let source = "\
import kotlin.reflect.typeOf

class A
class B

fun box(): String {
    val sugar = typeOf<(A) -> B>()
    val named = typeOf<Function1<A, B>>()
    if (sugar != named) return \"equal: $sugar $named\"
    if (sugar.hashCode() != named.hashCode()) return \"hash\"
    if (sugar.toString() != named.toString()) return \"text: $sugar $named\"
    if (typeOf<(A) -> B>() == typeOf<(B) -> A>()) return \"argument order\"
    return \"OK\"
}
";
    common::expect_box_ok_with_stdlib(source, "FunctionTypeOfJvm");
    common::expect_native_box(source, "FunctionTypeOfNative", "OK");
}
