//! A nested call's open formal that shares an enclosing type parameter's name stays the nested
//! call's own variable, solved by the enclosing call's expected type, when the formal comes from a
//! library callee and keeps its bare name, and when an expression body without a declared return
//! type has its return inferred before the body is checked (kotlinc 2.4.21).
//!
//! PLATFORM-SPECIFIC: `kotlin.Pair` construction and the `emptySet` body are not yet available on
//! the Wasm backend; the portable source-declared form is `nestedResultOfEnclosingTypeParameter.kt`.

use super::common;

#[test]
fn a_library_formal_named_like_an_enclosing_type_parameter_stays_open() {
    common::expect_box_same_as_kotlinc(
        "fun take(p: Pair<Any, Set<Any>>): Int = p.second.size\n\
         fun <B> inferred() = take(\"OK\" to emptySet())\n\
         fun <T, B> declared(): Int = take(\"OK\" to emptySet())\n\
         fun box(): String {\n\
             if (inferred<Int>() != 0) return \"FAIL inferred\"\n\
             if (declared<Int, String>() != 0) return \"FAIL declared\"\n\
             return \"OK\"\n\
         }\n",
        "NestedLibraryFormalNamedLikeEnclosing",
    );
}
