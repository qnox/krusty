//! Floating `==`/`!=` between non-null operands materializes its IEEE 754 Boolean before any jump
//! consumes it, and `!=` negates that Boolean, as kotlinc's `Ieee754Equals` intrinsic does.
use super::common;

const SOURCE: &str = "fun ne(x: Double, y: Double) = x != y\n\
                      fun eq(x: Float, y: Float) = x == y\n\
                      fun cast(x: Any, y: Any): Boolean = (x as Double) != (y as Double)\n\
                      fun branch(x: Double, y: Double): String { if (x != y) return \"ne\"; return \"eq\" }\n\
                      fun smart(x: Any, y: Double): String { if (x is Double && x == y) return \"eq\"; return \"ne\" }\n\
                      fun loop(x: Double, y: Double): String { while (x != y) { return \"ne\" }; return \"eq\" }\n\
                      fun negated(x: Double, y: Double): String = if (!(x == y)) \"ne\" else \"eq\"\n\
                      fun subject(x: Float, y: Float): String = when { x != y -> \"ne\"; else -> \"eq\" }\n";

#[test]
fn floating_equality_materializes_its_boolean_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "FloatingEquality",
        SOURCE,
        "FloatingEqualityKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("FloatingEqualityKt differs from kotlinc:\n{diff}"));
}

#[test]
fn floating_equality_follows_ieee_754() {
    let src = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   val nan = 0.0 / 0.0\n\
         \x20   if (!ne(nan, nan)) return \"nan ne\"\n\
         \x20   if (eq(-0.0f, 0.0f) != true) return \"zero eq\"\n\
         \x20   if (cast(1.0, 1.0)) return \"cast\"\n\
         \x20   if (branch(nan, nan) != \"ne\") return \"branch\"\n\
         \x20   if (smart(2.0, 2.0) != \"eq\") return \"smart\"\n\
         \x20   if (loop(1.0, 2.0) != \"ne\") return \"loop\"\n\
         \x20   if (negated(-0.0, 0.0) != \"eq\") return \"negated\"\n\
         \x20   if (subject(1.0f, 1.0f) != \"eq\") return \"subject\"\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    let actual =
        common::compile_and_run_box(&src, "floating_equality", &[common::stdlib_jar()], None)
            .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}
