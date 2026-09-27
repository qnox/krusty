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
