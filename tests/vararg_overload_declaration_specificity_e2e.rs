//! Overloads that both apply and whose instantiated parameters do not order are ranked by their
//! declarations, each general side's type parameters open, as kotlinc ranks them. The same holds
//! where a declaration's result type is inferred, whose call selection runs before bodies.
use super::common;

#[test]
fn a_collection_overload_beats_a_vararg_one_by_declaration_like_kotlinc() {
    let source = include_str!("box_regressions/varargOverloadDeclarationSpecificity.kt");
    common::expect_box_ok_with_stdlib(source, "VarargOverloadDeclarationSpecificity");
}

/// Without type parameters there is nothing to solve: `Collection<Int>?` and the vararg element
/// `Any` do not order, and kotlinc reports the call ambiguous, in an inferred result as anywhere.
#[test]
fn non_generic_collection_and_vararg_overloads_stay_ambiguous_like_kotlinc() {
    let source = "class K {\n\
                  \x20   fun h(e: Collection<Int>?): Int = 1\n\
                  \x20   fun h(vararg e: Any): Int = 1\n\
                  }\n\
                  fun inferred(k: K, l: List<Int>) = k.h(l)\n";
    common::assert_error_blocks_match_kotlinc(&[("main.kt", source)], &[]);
}
