//! An exhaustive `when` without an `else` throws `NoWhenBranchMatchedException` on the `when`'s
//! own line, where fir2ir builds the implicit `else` branch.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      sealed class S\n\
                      class X : S()\n\
                      class Y : S()\n\
                      \n\
                      fun value(s: S): Int {\n\
                      \x20   return when (s) {\n\
                      \x20       is X -> 1\n\
                      \x20       is Y -> 2\n\
                      \x20   }\n\
                      }\n\
                      \n\
                      fun statement(s: S, out: IntArray) {\n\
                      \x20   when (s) {\n\
                      \x20       is X -> out[0] = 1\n\
                      \x20       is Y -> out[0] = 2\n\
                      \x20   }\n\
                      }\n\
                      \n\
                      fun flag(b: Boolean): Int = when (b) {\n\
                      \x20   true -> 1\n\
                      \x20   false -> 0\n\
                      }\n\
                      \n\
                      fun multiline(s: S): Int =\n\
                      \x20   when (\n\
                      \x20       s\n\
                      \x20   ) {\n\
                      \x20       is X -> 1\n\
                      \x20       is Y -> 2\n\
                      \x20   }\n";

#[test]
fn no_when_branch_matched_throws_on_the_when_line_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "WhenNoBranchMatchedLine",
        SOURCE,
        "store/WhenNoBranchMatchedLineKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/WhenNoBranchMatchedLineKt differs from kotlinc: {diff}"));
}
