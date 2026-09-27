//! A captured mutable local's declaration takes kotlinc's `SharedVariablesLowering` shape: the local
//! stores a fresh `Ref` holder, then a separate statement sets its `element` to the initial value,
//! on the declaration's line. A declaration without an initializer (`lateinit`) leaves the element
//! unset, and the holder's debug range opens at its store.
use super::common;

const SOURCE: &str = "fun run1(f: () -> Unit) = f()\n\
    fun reference(): String { var result = \"\"; run1 { result = \"OK\" }; return result }\n\
    fun primitive(): Int { var n = 1; run1 { n = 2 }; return n }\n\
    fun wide(): Long { var n = 1L; run1 { n = 2L }; return n }\n\
    fun multiline(): String {\n\
    \x20   var result =\n\
    \x20       \"x\"\n\
    \x20   run1 { result = \"OK\" }\n\
    \x20   return result\n\
    }\n\
    fun late(): String { lateinit var s: String; run1 { s = \"q\" }; return \"late\" }\n";

#[test]
fn a_captured_local_stores_its_holder_before_setting_the_element() {
    common::byte_diff_against_kotlinc_cp(
        "SharedCellDeclaration",
        SOURCE,
        "SharedCellDeclarationKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("SharedCellDeclarationKt differs from kotlinc:\n{diff}"));
}

#[test]
fn a_captured_local_reads_back_what_its_closure_wrote() {
    let src = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   if (primitive() != 2) return \"primitive\"\n\
         \x20   if (wide() != 2L) return \"wide\"\n\
         \x20   if (multiline() != \"OK\") return \"multiline\"\n\
         \x20   if (late() != \"late\") return \"late\"\n\
         \x20   return reference()\n\
         }}\n"
    );
    let actual = common::compile_and_run_box(
        &src,
        "shared_cell_declaration",
        &[common::stdlib_jar()],
        None,
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}

const HOST: &str = "package host\n\ninline fun <T> hosted(f: () -> T): T = f()\n";

/// A literal an inline function expands still lets the holder escape when its body creates an
/// ordinary closure over the local, so the declaration keeps the holder-first shape. The host is
/// this repository's own inline function, compiled by kotlinc.
const NESTED_CLOSURE: &str = "import host.*\n\
    fun nested(): Int {\n\
    \x20   var x = 1\n\
    \x20   hosted { val g = { x += 2 }; g() }\n\
    \x20   return x\n\
    }\n";

/// `nested`'s code and line numbers match kotlinc's, and so does the holder's debug range, which
/// opens at the holder's store. The order of the other local-variable rows (the inline markers
/// and `g`) is the inliner's, so only the holder's row is compared.
#[test]
fn a_nested_closure_inside_an_inlined_literal_keeps_the_holder() {
    let classes =
        common::classes_against_kotlinc_lib("NestedClosure", &[("Host.kt", HOST)], NESTED_CLOSURE)
            .expect("the reference kotlinc is provisioned");
    let (reference, krusty) =
        classes.method_listing("NestedClosureKt", "public static final int nested();");
    let split = |listing: &str| -> (String, String) {
        let (code, locals) = listing
            .split_once("LocalVariableTable:")
            .unwrap_or_else(|| panic!("nested has no local variables:\n{listing}"));
        let holder = locals
            .lines()
            .filter(|row| row.ends_with(" x Lkotlin/jvm/internal/Ref$IntRef;"))
            .collect::<Vec<_>>()
            .join("\n");
        (code.to_string(), holder)
    };
    let (reference_code, reference_holder) = split(&reference);
    let (krusty_code, krusty_holder) = split(&krusty);
    assert_eq!(
        krusty_code, reference_code,
        "\n--- kotlinc ---\n{reference}\n--- krusty ---\n{krusty}"
    );
    assert_eq!(reference_holder.lines().count(), 1, "{reference}");
    assert_eq!(krusty_holder, reference_holder);
}

#[test]
fn a_nested_closure_inside_an_inlined_literal_writes_through_the_holder() {
    let main = format!(
        "{NESTED_CLOSURE}\
         fun box(): String = if (nested() == 3) \"OK\" else \"nested \" + nested()\n"
    );
    let output = common::expect_box_run_against_ref("shared_cell_nested_closure", HOST, &main)
        .expect("the reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}
