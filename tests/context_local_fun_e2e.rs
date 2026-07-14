//! Context parameters on LOCAL functions: `context(s: String) fun f() = s` declared inside a
//! function body. The parser consumes the `context(...)` prefix before a local `fun` (buffering the
//! params as leading value parameters), the resolver fills each leading context parameter from an
//! in-scope source at the call site, and the lowerer prepends those sources to the lifted method
//! call. Verified end-to-end against the reference box corpus.

use crate::common;

fn check(rel: &str) {
    match common::run_box_corpus_case(rel) {
        Some(s) => assert_eq!(s, "OK", "{rel}"),
        None => panic!("unexpectedly skipped: {rel}"),
    }
}

#[test]
fn contextual_local_function() {
    check("contextParameters/contextualLocalFunction.kt");
}

#[test]
fn contextual_local_fun_with_type_param() {
    check("contextParameters/contextualLocalFunWithTypeParam.kt");
}

#[test]
fn contextual_local_fun_and_top_level_fun() {
    check("contextParameters/contextualLocalFunAndTopLevelFun.kt");
}

#[test]
fn same_name_with_local_value_parameter() {
    check("contextParameters/sameNameWithLocalValueParameter.kt");
}
