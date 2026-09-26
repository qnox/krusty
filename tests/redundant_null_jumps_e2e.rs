//! kotlinc's `RedundantNullCheckMethodTransformer`, its null jumps and `instanceof`s: a jump on
//! whether a value known to be `null` (or known not to be) is `null` goes or becomes a `goto`, and
//! an `instanceof` of `null`, or of a new object of exactly the tested class, is a constant.
//!
//! krusty kept `aload; ifnull` of a local copied from a checked parameter, and `aload; instanceof`
//! of a local holding `null` or a new `Box`.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};
use super::temporary_elimination_e2e::stack_map;

const SOURCE: &str = "class Box\n\
    inline fun orDefault(x: String?): String = x ?: \"d\"\n\
    inline fun isBox(x: Any?): Boolean = x is Box\n\
    fun elvisOnKnown(): String = orDefault(\"a\")\n\
    fun isOfNull(): Boolean = isBox(null)\n\
    fun testOfCheckedCopy(s: String): Int {\n\
    \x20   val t: String? = s\n\
    \x20   var r = 0\n\
    \x20   if (t != null) r = 1\n\
    \x20   return r\n\
    }\n\
    fun isOfNullLocal(): Boolean {\n\
    \x20   val t: Any? = null\n\
    \x20   return t is Box\n\
    }\n\
    fun isOfNew(): Boolean {\n\
    \x20   val b: Any = Box()\n\
    \x20   return b is Box\n\
    }\n\
    fun equalsNullOfCheckedCopy(s: String): Int {\n\
    \x20   val t: String? = s\n\
    \x20   return if (t == null) 0 else 1\n\
    }\n";

#[test]
fn a_null_jump_or_instanceof_on_a_known_value_folds_like_kotlincs() {
    let built = compare_with_kotlinc_plugin(
        "NullJumps",
        SOURCE,
        "NullJumpsKt",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are required");
    // The inline samples (`elvisOnKnown`, `isOfNull`) fold too, but still differ from kotlinc
    // elsewhere: kotlinc keeps a `$i$f$` marker local per inlined call, and its mid-pipeline
    // dead-code step lets the `goto` left by a folded jump (`equalsNullOfCheckedCopy`) go.
    for member in [
        "int testOfCheckedCopy(java.lang.String)",
        "boolean isOfNullLocal()",
        "boolean isOfNew()",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
        assert_eq!(
            stack_map(&built.krusty, member),
            stack_map(&built.reference, member),
            "{member} frames"
        );
    }
    // What is left of each fold in the samples whose other differences are not this pass's.
    for (member, folded) in [
        ("java.lang.String elvisOnKnown()", "ifnonnull"),
        ("boolean isOfNull()", "instanceof"),
        ("int equalsNullOfCheckedCopy(java.lang.String)", "ifnonnull"),
    ] {
        let krusty = method_instructions(&built.krusty, member);
        assert!(!krusty.is_empty(), "{member} not found");
        assert!(
            !method_instructions(&built.reference, member)
                .iter()
                .chain(&krusty)
                .any(|row| row.starts_with(folded)),
            "{member} still has its {folded}: {krusty:?}"
        );
    }
}

/// Folding changes nothing that runs: each sample returns what it returned with its tests.
#[test]
fn folded_null_jumps_and_instanceofs_still_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (elvisOnKnown() !== \"a\") return \"elvis\"\n\
             \x20   if (isOfNull()) return \"isOfNull\"\n\
             \x20   if (testOfCheckedCopy(\"s\") != 1) return \"testOfCheckedCopy\"\n\
             \x20   if (isOfNullLocal()) return \"isOfNullLocal\"\n\
             \x20   if (!isOfNew()) return \"isOfNew\"\n\
             \x20   return if (equalsNullOfCheckedCopy(\"s\") == 1) \"OK\" else \"equalsNull\"\n\
             }}\n"
        ),
        "NullJumpsBox",
    );
}
