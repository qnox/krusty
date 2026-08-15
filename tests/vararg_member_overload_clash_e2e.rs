//! A `vararg` parameter erases to an ARRAY on the JVM, so `of(e: Int)` and `of(vararg a: Int)` have
//! different descriptors — `(I)` and `([I)` — and are legal overloads.
//!
//! The clash key keyed a parameter by its declared type alone, which made every same-name pair of
//! "one element" and "vararg of that element" collide. Top-level functions were fixed once; class
//! and companion members went through the same key and still reported
//! "conflicting overloads: function 'of' has the same JVM signature as another after type erasure".
//! Found on intellij-community's `fleet.fastutil` `IntList`/`IntOpenHashSet`, whose companions
//! declare `of()`, `of(e)`, `of(e0, e1)`, `of(e0, e1, e2)` and `of(vararg a)` side by side — the
//! JetBrains Kotlin language server reports nothing there.

use super::common;

#[test]
fn companion_vararg_and_element_overloads_coexist() {
    let src = "class IntList {\n\
\x20   companion object {\n\
\x20       fun of(): String = \"none\"\n\
\x20       fun of(e: Int): String = \"one:\" + e\n\
\x20       fun of(e0: Int, e1: Int): String = \"two:\" + (e0 + e1)\n\
\x20       fun of(vararg a: Int): String = \"many:\" + a.size\n\
\x20   }\n\
}\n\
fun box(): String {\n\
\x20   if (IntList.of() != \"none\") return \"f1: \" + IntList.of()\n\
\x20   if (IntList.of(7) != \"one:7\") return \"f2: \" + IntList.of(7)\n\
\x20   if (IntList.of(1, 2) != \"two:3\") return \"f3: \" + IntList.of(1, 2)\n\
\x20   if (IntList.of(1, 2, 3) != \"many:3\") return \"f4: \" + IntList.of(1, 2, 3)\n\
\x20   return \"OK\"\n\
}\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("VarargCompanionOverloadReference", src);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected the companion overload fixture: {reference_stderr}"
    );
    common::expect_box_ok_with_stdlib(src, "VMOC");
}

#[test]
fn member_vararg_and_element_overloads_coexist() {
    let src = "class Sink {\n\
\x20   fun take(value: String): String = \"one:\" + value\n\
\x20   fun take(vararg values: String): String = \"many:\" + values.size\n\
}\n\
fun box(): String {\n\
\x20   val s = Sink()\n\
\x20   if (s.take(\"a\") != \"one:a\") return \"f1: \" + s.take(\"a\")\n\
\x20   if (s.take(\"a\", \"b\") != \"many:2\") return \"f2: \" + s.take(\"a\", \"b\")\n\
\x20   return \"OK\"\n\
}\n";
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("VarargMemberOverloadReference", src);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected the member overload fixture: {reference_stderr}"
    );
    common::expect_box_ok_with_stdlib(src, "VMOM");
}

#[test]
fn member_array_and_vararg_of_the_same_element_still_clash() {
    // `Array<String>` and `vararg String` erase to the SAME descriptor, so this pair is a real
    // conflict — the fix must not stop reporting it.
    let src = "class Sink {\n\
\x20   fun take(values: Array<String>): String = \"array\"\n\
\x20   fun take(vararg values: String): String = \"vararg\"\n\
}\n\
fun box(): String = Sink().take(\"a\")\n";
    let (reference_code, _) = common::kotlinc_source_result("VarargArrayClashReference", src);
    assert_ne!(
        reference_code, 0,
        "kotlinc unexpectedly accepted the Array<String>/vararg String clash"
    );
    let diagnostics = common::checker_diags_with_stdlib(src).expect("checker ran");
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("conflicting overloads")),
        "an Array<String>/vararg String pair must still be reported: {diagnostics:?}"
    );
}
