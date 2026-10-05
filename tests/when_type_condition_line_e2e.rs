//! A subject `when`'s `is` conditions read the subject at the condition's own line, so the line
//! of a branch condition on its own line lands on its `instanceof` (or on the subject load when
//! the subject's temporary survives), as kotlinc's per-condition `tmp_subject` reads put it.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      class Bob {\n\
                      \x20   fun bar(): String = \"OK\"\n\
                      }\n\
                      class Ann\n\
                      \n\
                      fun parameter(x: Any): String = when (x) {\n\
                      \x20   is Bob -> x.bar()\n\
                      \x20   else -> \"no\"\n\
                      }\n\
                      \n\
                      fun Any.receiver(): String = when (this) {\n\
                      \x20   is Bob -> bar()\n\
                      \x20   else -> \"no\"\n\
                      }\n\
                      \n\
                      fun negated(x: Any): String = when (x) {\n\
                      \x20   !is Bob -> \"nb\"\n\
                      \x20   else -> \"b\"\n\
                      }\n\
                      \n\
                      fun mixed(x: Any): String = when (x) {\n\
                      \x20   \"s\" -> \"str\"\n\
                      \x20   is Bob -> x.bar()\n\
                      \x20   else -> \"no\"\n\
                      }\n\
                      \n\
                      fun both(x: Any): String = when (x) {\n\
                      \x20   is Bob,\n\
                      \x20   is Ann -> \"named\"\n\
                      \x20   else -> \"no\"\n\
                      }\n\
                      \n\
                      fun guarded(x: Any, flag: Boolean): String = when (x) {\n\
                      \x20   is Bob if flag -> \"bob\"\n\
                      \x20   else -> \"no\"\n\
                      }\n";

#[test]
fn when_type_conditions_mark_their_lines_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "WhenTypeConditionLine",
        SOURCE,
        "store/WhenTypeConditionLineKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/WhenTypeConditionLineKt differs from kotlinc: {diff}"));
}
