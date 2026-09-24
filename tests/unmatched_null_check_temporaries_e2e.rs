//! A null check kotlinc's safe-call rewrite cannot match leaves the method's temporaries to fold.
//!
//! The class writer applies kotlinc's temporary elimination only when it can apply the whole of
//! kotlinc's coupled safe-call rewrite (`simplifyKnownSafeCallPatterns`), and it gave up on the
//! method at any `aload; ifnull`/`ifnonnull` pair it had not rewritten. Most such pairs are ones
//! kotlinc's matcher does not match either: `if (x == null) return` jumps to a label after a return,
//! which kotlinc's dead `nop` also reaches. So `println(label(n))` after such a guard kept its
//! `astore; getstatic System.out; aload` where kotlinc has `getstatic System.out; swap`.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "fun label(n: Int): String = \"n=\" + n\n\
    fun guarded(x: String?, n: Int) {\n\
    \x20   if (x == null) return\n\
    \x20   println(label(n))\n\
    }\n";

#[test]
fn an_unmatched_null_check_leaves_the_temporaries_to_fold() {
    let Some(built) = compare_with_kotlinc_plugin(
        "UnmatchedNullCheckTemporaries",
        SOURCE,
        "UnmatchedNullCheckTemporariesKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let member = "void guarded(";
    let reference = method_instructions(&built.reference, member);
    assert!(
        reference.iter().any(|insn| insn.ends_with("swap")),
        "kotlinc's folded temporary: {reference:?}"
    );
    assert_eq!(
        method_instructions(&built.krusty, member),
        reference,
        "{member}"
    );
}
