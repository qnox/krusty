//! `CollectionLiterals` (`-Xcollection-literals`): `[…]` outside annotations and the `operator fun
//! of` convention. Without the feature kotlinc reports `UNSUPPORTED_FEATURE` at the `operator`
//! modifier of an `of` function, and at each literal outside an annotation both
//! `UNSUPPORTED_ARRAY_LITERAL_OUTSIDE_OF_ANNOTATION` and `UNSUPPORTED_FEATURE`, after any type
//! mismatch of the literal: it types the literal as the array of its elements' common supertype
//! (the expected element type when it has none, a primitive array when one is expected). An
//! annotation argument and an annotation class's parameter default stay array literals. The
//! complete error ledger must be the expected one for both compilers, and with the argument both
//! run the fixture to "OK".

use super::common;

const UNSUPPORTED: &str = "the feature \"collection literals\" is experimental and should be \
    enabled explicitly. This can be done by supplying the compiler argument \
    '-Xcollection-literals', but note that no stability guarantees are provided.";
const OUTSIDE: &str = "array literals outside of annotations are unsupported.";

fn literal(at: &str) -> [String; 2] {
    [
        format!("Main.kt:{at}: {OUTSIDE}"),
        format!("Main.kt:{at}: {UNSUPPORTED}"),
    ]
}

fn error(at: &str, message: &str) -> String {
    format!("Main.kt:{at}: {message}")
}

fn assert_ledger(source: &str, expected: &[String]) {
    let sources = [("Main.kt", source)];
    assert_eq!(
        common::reference_error_ledger(&sources, &[]),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::krusty_error_ledger_with_args(&sources, &[]),
        expected
    );
}

const FACTORIES: &str = r#"class MyList(val data: Array<out String>) {
    companion object {
        operator fun of(vararg strs: String) = MyList(strs)
    }
}
fun take(l: MyList) = l.data.size
annotation class Ann(val xs: IntArray = [1])
@Ann([2]) fun annotated() {}
fun box(): String {
    val a: MyList = ["a"]
    val e: List<Int> = [1, 2]
    val s: Set<String> = []
    return if (a.data[0] == "a" && take(["x", "y"]) == 2 && e == listOf(1, 2) && s.isEmpty()) "OK" else "fail"
}
"#;

#[test]
fn collection_literals_require_the_feature() {
    let mut expected = vec![error("3:9", UNSUPPORTED)];
    expected.push(error(
        "10:19",
        "initializer type mismatch: expected 'MyList', actual 'Array<String>'.",
    ));
    expected.extend(literal("10:21"));
    expected.push(error(
        "11:22",
        "initializer type mismatch: expected 'List<Int>', actual 'Array<Int>'.",
    ));
    expected.extend(literal("11:24"));
    expected.push(error(
        "12:24",
        "initializer type mismatch: expected 'Set<String>', actual 'Array<Any?>'.",
    ));
    expected.extend(literal("12:26"));
    expected.push(error(
        "13:41",
        "argument type mismatch: actual type is 'Array<String>', but 'MyList' was expected.",
    ));
    expected.extend(literal("13:41"));
    assert_ledger(FACTORIES, &expected);
}

#[test]
fn collection_literals_run_with_the_argument() {
    common::expect_box_same_as_kotlinc_with_args(
        FACTORIES,
        "CollectionLiterals",
        &["-Xcollection-literals"],
    );
}

/// Array-typed targets: the literal is the expected array when its elements fit, a primitive array
/// when one is expected, and otherwise the array of its elements' common supertype.
const ARRAYS: &str = r#"fun box(): String {
    val a: Array<String> = ["a"]
    val b: IntArray = [1, 2]
    val c: Array<Int?> = [1]
    val d = ["z"]
    val f: Array<String> = []
    return "OK"
}
"#;

#[test]
fn array_literals_outside_annotations_are_typed_as_arrays() {
    let mut expected = Vec::new();
    expected.extend(literal("2:28"));
    expected.extend(literal("3:23"));
    expected.push(error(
        "4:24",
        "initializer type mismatch: expected 'Array<Int?>', actual 'Array<Int>'.",
    ));
    expected.extend(literal("4:26"));
    expected.extend(literal("5:13"));
    expected.extend(literal("6:28"));
    assert_ledger(ARRAYS, &expected);
}

/// A literal nested anywhere in an annotation argument, or in an annotation class's parameter
/// default, is in an annotation context and stays an array literal without the feature.
const ANNOTATION_CONTEXTS: &str = r#"annotation class Strings(val v: Array<String> = arrayOf(*["a"]))
annotation class Ints(val v: IntArray = [1])
@Strings(arrayOf(*["b"], "c"))
@Ints([2])
fun annotated() {}
"#;

#[test]
fn literals_in_annotation_contexts_need_no_feature() {
    common::assert_sources_accepted_like_kotlinc(&[("Main.kt", ANNOTATION_CONTEXTS)], &[]);
}
