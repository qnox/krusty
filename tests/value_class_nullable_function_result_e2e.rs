//! A function declared to return `X?`, where `X` is a value class over a non-null reference,
//! returns the carrier (`String` for `Tag(val name: String)`). A function value's `invoke` hands
//! back the box through the `FunctionN` `Object` slot, so the return unboxes it null-safely, as
//! kotlinc's safe `unbox-impl` call: `checkcast Tag; dup; ifnull; unbox-impl; goto; pop;
//! aconst_null`. Without it the box itself is returned where the carrier is declared, and the
//! verifier rejects the method.

use super::common;

const SRC: &str = "@JvmInline value class Tag(val name: String)\n\
    fun apply(f: (Tag?) -> Tag?, t: Tag?): Tag? = f(t)\n\
    fun same(t: Tag?): Tag? = t\n\
    fun none(t: Tag?): Tag? = null\n\
    fun box(): String {\n\
    \x20   if (apply(::same, Tag(\"O\"))?.name != \"O\") return \"same\"\n\
    \x20   if (apply(::none, Tag(\"x\")) != null) return \"none\"\n\
    \x20   if (apply(::same, null) != null) return \"null argument\"\n\
    \x20   return \"OK\"\n\
    }\n";

#[test]
fn a_nullable_value_class_return_unboxes_a_function_result_like_kotlinc() {
    let built = common::compare_with_kotlinc_plugin(
        "NullableValueClassFunctionResult",
        SRC,
        "NullableValueClassFunctionResultKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let member = "apply-YyfE6XM(";
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits apply");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference
    );
}

#[test]
fn a_nullable_value_class_function_result_runs() {
    common::expect_box_ok_with_stdlib(SRC, "NullableValueClassFunctionResult");
}
