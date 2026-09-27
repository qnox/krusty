//! A private member is realized as a private JVM method; a companion (another JVM class) reaches it
//! through kotlinc's `public static final synthetic access$<name>` bridge over the member's own
//! parameters, since a cross-class private invocation fails with `IllegalAccessError`. A value
//! class's member is the static `<name>-impl`, so its bridge is `access$<name>-impl`. A private
//! property with a declared getter is bridged the same way, through its getter.

use super::common;

const FUNCTION_SRC: &str = "@JvmInline value class Weight(private val grams: Int) {\n\
    \x20   private fun heavy(): String = if (grams > 10) \"heavy\" else \"OK\"\n\
    \x20   companion object {\n\
    \x20       fun check(weight: Weight): String = weight.heavy()\n\
    \x20   }\n\
    }\n\
    fun box(): String = Weight.check(Weight(0))\n";

const PROPERTY_SRC: &str = "@JvmInline value class Weight(private val grams: Int) {\n\
    \x20   private val heavy: String get() = if (grams > 10) \"heavy\" else \"OK\"\n\
    \x20   companion object {\n\
    \x20       fun check(weight: Weight): String = weight.heavy\n\
    \x20   }\n\
    }\n\
    fun box(): String = Weight.check(Weight(0))\n";

const CLASS_PROPERTY_SRC: &str = "class Holder(private val grams: Int) {\n\
    \x20   private val heavy: String get() = if (grams > 10) \"heavy\" else \"OK\"\n\
    \x20   companion object {\n\
    \x20       fun check(holder: Holder): String = holder.heavy\n\
    \x20   }\n\
    }\n\
    fun box(): String = Holder.check(Holder(0))\n";

/// `member` of `class` matches kotlinc: its instructions with every call target, and its locals.
fn assert_same_method(src: &str, class: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "PrivateMemberAccess",
        src,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, &format!("{member};"));
    assert!(!reference.is_empty(), "kotlinc emits {class}.{member}");
    assert_eq!(
        common::method_instructions(&built.krusty, &format!("{member};")),
        reference,
        "{class}.{member}"
    );
    match common::method_code_diff_against_kotlinc("Main", &[], src, class, member) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("{class}.{member}: reference toolchain unavailable"),
    }
}

fn assert_runs(src: &str) {
    let output = common::compile_and_run_box(src, "Main", &[common::stdlib_jar()], None)
        .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}

#[test]
fn the_private_impl_gets_a_static_access_bridge() {
    assert_same_method(
        FUNCTION_SRC,
        "Weight",
        "public static final java.lang.String access$heavy-impl(int)",
    );
}

#[test]
fn the_companion_calls_the_bridge() {
    assert_same_method(
        FUNCTION_SRC,
        "Weight$Companion",
        "public final java.lang.String check-Cm3DN5A(int)",
    );
}

#[test]
fn a_companion_calls_a_private_value_class_member() {
    assert_runs(FUNCTION_SRC);
}

#[test]
fn a_private_value_class_getter_gets_a_static_access_bridge() {
    assert_same_method(
        PROPERTY_SRC,
        "Weight",
        "public static final java.lang.String access$getHeavy-impl(int)",
    );
}

#[test]
fn the_companion_reads_the_property_through_the_bridge() {
    assert_same_method(
        PROPERTY_SRC,
        "Weight$Companion",
        "public final java.lang.String check-Cm3DN5A(int)",
    );
}

#[test]
fn a_companion_reads_a_private_value_class_property() {
    assert_runs(PROPERTY_SRC);
}

#[test]
fn a_private_class_getter_gets_an_instance_access_bridge() {
    assert_same_method(
        CLASS_PROPERTY_SRC,
        "Holder",
        "public static final java.lang.String access$getHeavy(Holder)",
    );
}

#[test]
fn the_companion_reads_the_class_property_through_the_bridge() {
    assert_same_method(
        CLASS_PROPERTY_SRC,
        "Holder$Companion",
        "public final java.lang.String check(Holder)",
    );
}

#[test]
fn a_companion_reads_a_private_class_property() {
    assert_runs(CLASS_PROPERTY_SRC);
}
