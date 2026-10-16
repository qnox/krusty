//! A companion property hoisted to an outer static is initialized by the outer `<clinit>`, so the
//! lambdas its initializer creates are private statics of the outer class, as kotlinc places them.

use super::common;

const SOURCE: &str = r#"
fun plain(f: () -> String): String = f()

class Holder {
    companion object {
        val shared = plain { "O" }
        val nested = plain { plain { "K" } }
    }
}

fun box(): String = Holder.shared + Holder.nested
"#;

#[test]
fn a_companion_initializer_lambda_is_a_method_of_the_outer_class() {
    common::expect_box_same_as_kotlinc(SOURCE, "CompanionInitializerLambda");
    for class in ["Holder", "Holder$Companion"] {
        common::byte_diff_against_kotlinc_cp(
            "CompanionInitializerLambda",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
