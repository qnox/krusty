//! A library function's `Tag?` parameter or result, where `Tag` wraps a non-null reference, is
//! `String` in its JVM descriptor: kotlinc erases `Tag?` to the carrier, since the carrier's own
//! null stands for the absent value. `@Metadata` still names `Tag?`, and the caller reads that type,
//! passing a `Tag` or `null` and receiving a `Tag?`. Read as the descriptor's `String` instead, the
//! caller's `Tag` argument and `null` were rejected.

use super::common;

const LIB: &str = "package lib\n\
    @JvmInline value class Tag(val name: String)\n\
    fun take(tag: Tag?): String = if (tag == null) \"none\" else tag.name\n\
    inline fun nameOf(tag: Tag?): String = if (tag == null) \"none\" else tag.name\n\
    fun give(name: String?): Tag? = if (name == null) null else Tag(name)\n";

const MAIN: &str = "import lib.*\n\
    fun taken(): String = take(Tag(\"t\"))\n\
    fun untaken(): String = take(null)\n\
    fun named(): String = nameOf(Tag(\"n\"))\n\
    fun unnamed(): String = nameOf(null)\n\
    fun given(): String? = give(\"g\")?.name\n\
    fun ungiven(): Tag? = give(null)\n\
    fun box(): String {\n\
    \x20   if (taken() != \"t\") return \"taken\"\n\
    \x20   if (untaken() != \"none\") return \"untaken\"\n\
    \x20   if (named() != \"n\") return \"named\"\n\
    \x20   if (unnamed() != \"none\") return \"unnamed\"\n\
    \x20   if (given() != \"g\") return \"given\"\n\
    \x20   if (ungiven() != null) return \"ungiven\"\n\
    \x20   return \"OK\"\n\
    }\n";

#[test]
fn library_nullable_value_class_signatures_compile_like_kotlinc() {
    let library =
        common::kotlinc_lib_out(&[("Lib.kt", LIB)]).expect("reference kotlinc is provisioned");
    let built = common::compare_with_kotlinc_plugin(
        "ClasspathNullableValueClass",
        MAIN,
        "ClasspathNullableValueClassKt",
        &[library, common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for function in ["taken", "untaken", "named", "unnamed", "given", "ungiven"] {
        let member = format!(" {function}();");
        let reference = common::method_instructions(&built.reference, &member);
        assert!(!reference.is_empty(), "kotlinc emits {function}");
        assert_eq!(
            common::method_instructions(&built.krusty, &member),
            reference,
            "{function}"
        );
    }
}

#[test]
fn library_nullable_value_class_signatures_run() {
    let output = common::expect_box_run_against_kotlinc(LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}
