//! A non-null cast of an immutable value reads it again, as kotlinc does.
//!
//! kotlinc's cast lowering checks `x as T` for `null` before casting. When `x` is a parameter or a
//! `val`, it reads `x` once for the check and again for the cast (`aload; ldc; checkNotNull; aload;
//! checkcast`); anything else is duplicated (`dup; ldc; checkNotNull; checkcast`). krusty
//! duplicated every operand. The lowering records which casts read an immutable value.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const SOURCE: &str = "fun make(): Any? = \"c\"\n\
    fun parameter(a: Any?): String = a as String\n\
    fun valLocal(a: Any?): String {\n\
    \x20   val v = a\n\
    \x20   return v as String\n\
    }\n\
    fun varLocal(a: Any?, b: Any?): String {\n\
    \x20   var v = a\n\
    \x20   v = b\n\
    \x20   return v as String\n\
    }\n\
    fun call(): String = make() as String\n";

#[test]
fn an_immutable_cast_operand_is_read_again_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "CastOperandReread",
        SOURCE,
        "CastOperandRereadKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // `varLocal` and `call` keep the `dup`: a `var` may change, a call must not run twice.
    for member in [
        "String parameter(",
        "String valLocal(",
        "String varLocal(",
        "String call(",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

#[test]
fn an_immutable_cast_operand_read_again_still_casts() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (parameter(\"a\") != \"a\" || valLocal(\"b\") != \"b\") return \"read\"\n\
             \x20   if (varLocal(null, \"x\") != \"x\" || call() != \"c\") return \"dup\"\n\
             \x20   try {{\n\
             \x20       parameter(null)\n\
             \x20       return \"null\"\n\
             \x20   }} catch (e: NullPointerException) {{\n\
             \x20   }}\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "cast operand read again",
    );
}
