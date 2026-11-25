//! A lambda parameter whose type is a value class over a primitive is not guarded, as in kotlinc,
//! even though the lambda implementation receives the box: kotlinc decides a guard from the
//! declared type unwrapped to its carrier, and a primitive carrier cannot be null.

use super::common;

const SOURCE: &str = "fun <T> T.runExt(fn: T.() -> String) = fn()\n\
                      fun <T> runWith(value: T, fn: (T) -> String) = fn(value)\n\
                      @JvmInline value class Primitive(private val r: Int) {\n\
                      \x20   fun receiver() = runExt { \"OK\" }\n\
                      \x20   fun parameter() = runWith(this) { \"OK\" }\n\
                      }\n\
                      @JvmInline value class Reference(private val r: String) {\n\
                      \x20   fun receiver() = runExt { \"OK\" }\n\
                      \x20   fun parameter() = runWith(this) { \"OK\" }\n\
                      }\n";

#[test]
fn a_lambda_parameter_over_a_primitive_value_class_is_not_guarded() {
    for class in ["Primitive", "Reference"] {
        common::byte_diff_against_kotlinc_cp(
            "ValueClassLambdaParameterGuard",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
    }
}
