//! Class literals `T::class` and `expr::class`, checked against JVM runtime behavior.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_array_literal_runs(tag: &str, source: &str) {
    let (reference_code, reference_stderr) = common::kotlinc_source_result(tag, source);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected {tag}: {reference_stderr}"
    );
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&[source]),
        Vec::<String>::new()
    );
    assert_eq!(common::expect_box_run_with_stdlib(source, tag), "OK");
}

#[test]
fn unbound_user_and_library_class_literals() {
    const SRC: &str = "class Foo\n\
fun box(): String {\n\
    val x: Any = Foo()\n\
    val s: Any = \"hi\"\n\
    if (x::class != Foo::class) return \"Fail 1\"\n\
    if (s::class != String::class) return \"Fail 2\"\n\
    if (x::class == s::class) return \"Fail 3\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("class literals"), "OK");
}

#[test]
fn primitive_class_literals_bound_and_unbound_agree() {
    // A primitive literal is modeled by its boxed wrapper class: `Int::class` (unbound) and `x::class`
    // (bound, boxed-then-getClass) compare equal, as do distinct primitives unequal.
    const SRC: &str = "fun box(): String {\n\
    val i = 42\n\
    val b = true\n\
    if (i::class != Int::class) return \"Fail 1\"\n\
    if (b::class != Boolean::class) return \"Fail 2\"\n\
    if (i::class == b::class) return \"Fail 3\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("primitive class literals"), "OK");
}

#[test]
fn class_literal_java_is_identity_on_the_class() {
    const SRC: &str = "class Foo\n\
fun box(): String {\n\
    if (Foo::class.java != Foo::class.java) return \"Fail 1\"\n\
    if (String::class.java.getName() != \"java.lang.String\") return \"Fail 2\"\n\
    val x: Any = Foo()\n\
    if (x::class.java != Foo::class.java) return \"Fail 3\"\n\
    if (String::class.java == Foo::class.java) return \"Fail 4\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("class literal .java"), "OK");
}

#[test]
fn class_literal_java_on_explicitly_imported_type() {
    const SRC: &str = "import java.util.ArrayList\n\
fun box(): String {\n\
    val c = ArrayList::class.java\n\
    if (c.getName() != \"java.util.ArrayList\") return \"Fail 1\"\n\
    if (c != ArrayList::class.java) return \"Fail 2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("class literal .java on imported type"),
        "OK"
    );
}

#[test]
fn bound_class_literal_smartcast_in_equals() {
    const SRC: &str = "class Foo(val s: String) {\n\
    override fun equals(other: Any?): Boolean {\n\
        return other != null && other::class == this::class && s == (other as Foo).s\n\
    }\n\
    override fun hashCode(): Int = s.hashCode()\n\
}\n\
fun box(): String = if (Foo(\"a\") == Foo(\"a\") && Foo(\"a\") != Foo(\"b\")) \"OK\" else \"Fail\"\n";
    assert_eq!(run(SRC).expect("bound class literal in equals"), "OK");
}

#[test]
fn array_class_literals() {
    const SRC: &str = "fun box(): String {\n\
    if (Array<String>::class.java.getName() != \"[Ljava.lang.String;\") return \"Fail 1\"\n\
    if (IntArray::class.java.getName() != \"[I\") return \"Fail 2\"\n\
    if (Array<Int>::class.java.getName() != \"[Ljava.lang.Integer;\") return \"Fail 3\"\n\
    if (Array<String>::class != Array<String>::class) return \"Fail 4\"\n\
    return \"OK\"\n\
}\n";
    assert_array_literal_runs("ArrayClassLiteral", SRC);
}

#[test]
fn nested_array_class_literal() {
    const SRC: &str = "fun box(): String =\n\
        if (Array<Array<String>>::class.java.getName() == \"[[Ljava.lang.String;\") \"OK\" else \"FAIL\"\n";
    assert_array_literal_runs("NestedArrayClassLiteral", SRC);
}

#[test]
fn qualified_array_class_literal() {
    const SRC: &str = "fun box(): String =\n\
        if (kotlin.Array<String>::class.java.getName() == \"[Ljava.lang.String;\") \"OK\" else \"FAIL\"\n";
    assert_array_literal_runs("QualifiedArrayClassLiteral", SRC);
}

#[test]
fn imported_alias_array_class_literal() {
    const SRC: &str = "import kotlin.Array as KotlinArray\n\
fun box(): String =\n\
    if (KotlinArray<String>::class.java.getName() == \"[Ljava.lang.String;\") \"OK\" else \"FAIL\"\n";
    assert_array_literal_runs("ImportedAliasArrayClassLiteral", SRC);
}

#[test]
fn user_array_classifier_is_not_the_builtin_array() {
    const SRC: &str = "class Array<T>\n\
fun f() { val literal = Array<String>::class }\n";
    let (reference_code, _) = common::kotlinc_source_result("UserArrayClassLiteral", SRC);
    assert_ne!(
        reference_code, 0,
        "kotlinc accepted a generic class literal"
    );
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&[SRC]),
        vec!["only classes are allowed on the left-hand side of a class literal.".to_string()]
    );
}

#[test]
fn typealias_array_class_literal() {
    const SRC: &str = "typealias Strings = Array<String>\n\
fun box(): String =\n\
    if (Strings::class.java.getName() == \"[Ljava.lang.String;\") \"OK\" else \"FAIL\"\n";
    assert_array_literal_runs("TypeAliasArrayClassLiteral", SRC);
}

#[test]
fn sibling_typealias_array_class_literal() {
    const ALIAS: &str = "package sample\ntypealias Strings = Array<String>\n";
    const MAIN: &str = "package sample\n\
fun box(): String =\n\
    if (Strings::class.java.getName() == \"[Ljava.lang.String;\") \"OK\" else \"FAIL\"\n";

    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&[ALIAS, MAIN]),
        Vec::<String>::new()
    );
    common::expect_box_ok_files_with_stdlib(
        &[("Alias.kt", ALIAS), ("Main.kt", MAIN)],
        "SiblingTypeAliasArrayClassLiteral",
    );
}

#[test]
fn classpath_typealias_array_class_literal() {
    const LIBRARY: &str = "package library\ntypealias Strings = Array<String>\n";
    const MAIN: &str = "import library.Strings\n\
fun box(): String =\n\
    if (Strings::class.java.getName() == \"[Ljava.lang.String;\") \"OK\" else \"FAIL\"\n";

    common::expect_box_ok_against("classpath_array_class_literal", LIBRARY, MAIN);
}

#[test]
fn array_class_literals_report_no_diagnostic() {
    let diags = common::front_end_diagnostics_files_with_stdlib(&["fun f() {\n\
         \u{20}   val a = Array<String>::class\n\
         \u{20}   val b = Array<String>::class.java\n\
         \u{20}   val c = IntArray::class\n\
         \u{20}   val d = IntArray::class.java\n\
         \u{20}   val e = Array<Array<String>>::class.java\n\
         \u{20}   val f = kotlin.Array<String>::class.java\n\
         }\n"]);
    assert_eq!(diags, Vec::<String>::new());
}
