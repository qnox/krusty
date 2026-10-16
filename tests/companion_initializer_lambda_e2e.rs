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

const SAME_NAME_SIBLINGS: &str = r#"
fun join(first: () -> String, second: () -> String): String = first() + second()

class Holder {
    val value = join({ "O" }, { "" })

    companion object {
        val value = join({ "K" }, { "" })
    }
}

fun box(): String = Holder().value + Holder.value
"#;

/// The expression walk that finds implementations is stack based, but physical `$lambda$N`
/// ordinals still follow source order when several moved siblings share one declaration name.
#[test]
fn moved_sibling_lambdas_keep_source_ordinal_order() {
    common::expect_box_same_as_kotlinc(
        SAME_NAME_SIBLINGS,
        "CompanionInitializerSiblingLambdaNames",
    );
    for class in ["Holder", "Holder$Companion"] {
        common::byte_diff_against_kotlinc_cp(
            "CompanionInitializerSiblingLambdaNames",
            SAME_NAME_SIBLINGS,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

const SAME_NAME_NESTED: &str = r#"
fun plain(f: () -> String): String = f()

class Holder {
    val value = plain { plain { "O" } }

    companion object {
        val value = plain { plain { "K" } }
    }
}

fun box(): String = Holder().value + Holder.value
"#;

/// A nested implementation follows its moved parent's physical ordinal: `value$lambda$1$0`, not
/// the companion's independent lexical `value$lambda$0$0` sequence.
#[test]
fn a_nested_moved_lambda_uses_its_parents_physical_ordinal() {
    common::expect_box_same_as_kotlinc(SAME_NAME_NESTED, "CompanionInitializerNestedLambdaNames");
    for class in ["Holder", "Holder$Companion"] {
        common::byte_diff_against_kotlinc_cp(
            "CompanionInitializerNestedLambdaNames",
            SAME_NAME_NESTED,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
