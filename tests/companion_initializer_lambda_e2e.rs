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

const SAME_NAME: &str = r#"
fun plain(f: () -> String): String = f()

class Holder {
    val value = plain { "O" }

    companion object {
        val value = plain { "K" }
    }
}

fun box(): String = Holder().value + Holder.value
"#;

/// A moved lambda continues the outer class's numbering for its name: a member and a companion
/// property of the same name get `value$lambda$0` and `value$lambda$1`, not one name twice.
#[test]
fn a_moved_companion_lambda_is_numbered_after_the_outer_class_lambdas() {
    common::expect_box_same_as_kotlinc(SAME_NAME, "CompanionInitializerLambdaName");
    for class in ["Holder", "Holder$Companion"] {
        common::byte_diff_against_kotlinc_cp(
            "CompanionInitializerLambdaName",
            SAME_NAME,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
