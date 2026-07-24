//! Kotlin 1.9 `data object`, in both positions.
//!
//! Two gaps had to close together. The class-body parser accepted `data class` but not `data object`
//! (its lookahead required `class`), so a nested one failed with "class bodies support member …" and the
//! whole file was skipped — while a TOP-LEVEL `data object` parsed fine, since that arm already treats
//! `data` as a modifier on either. And `synth_data_members` received an `is_object` flag but used it only
//! to skip `copy`: `toString` still built the data-CLASS shape, so a data object rendered `A()`.
//! kotlinc renders a data object as its BARE simple name, `A`.
//!
//! Fixing only the parser would have turned a clean skip into silently wrong output, so both land here.
//! A sibling `data class` in the same hierarchy still renders `P(n=3)`.
//!
//! `toString` is reached through an `Any` reference: resolving an `Any` member directly on an object
//! singleton (`S.A.toString()`) fails for PLAIN objects too — a separate, pre-existing gap.

use super::common;

#[test]
fn data_object_tostring_is_the_bare_simple_name() {
    let src = "sealed class S {\n\
        \x20   data object A : S()\n\
        \x20   data object B : S()\n\
        \x20   data class P(val n: Int) : S()\n\
        }\n\
        data object TopLevel\n\
        fun render(a: Any): String = a.toString()\n\
        fun box(): String {\n\
        \x20   if (render(S.A) != \"A\") return \"f1:\" + render(S.A)\n\
        \x20   if (render(S.B) != \"B\") return \"f2:\" + render(S.B)\n\
        \x20   if (render(S.P(3)) != \"P(n=3)\") return \"f3:\" + render(S.P(3))\n\
        \x20   if (render(TopLevel) != \"TopLevel\") return \"f4:\" + render(TopLevel)\n\
        \x20   return \"OK\"\n\
        }\n";
    common::assert_box_ok_with_stdlib(src, "DataObject");
}

#[test]
fn data_object_cases_are_matchable_and_distinct() {
    // The shape that motivated this: singleton cases of a sealed hierarchy, selected by `when` and
    // compared for equality (a data object is a singleton, so identity equality is the semantics).
    let src = "sealed class S {\n\
        \x20   data object A : S()\n\
        \x20   data object B : S()\n\
        \x20   data class P(val n: Int) : S()\n\
        }\n\
        fun name(s: S): String = when (s) {\n\
        \x20   is S.A -> \"A\"\n\
        \x20   is S.B -> \"B\"\n\
        \x20   is S.P -> \"P\" + s.n\n\
        }\n\
        fun box(): String {\n\
        \x20   if (name(S.A) != \"A\" || name(S.B) != \"B\" || name(S.P(7)) != \"P7\") return \"f1\"\n\
        \x20   if (S.A != S.A) return \"f2\"\n\
        \x20   if (S.A.hashCode() != S.A.hashCode()) return \"f3\"\n\
        \x20   return \"OK\"\n\
        }\n";
    common::assert_box_ok_with_stdlib(src, "DataObjectWhen");
}
