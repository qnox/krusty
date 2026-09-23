//! kotlinc's temporary-variable elimination, applied to krusty's finished methods.
//!
//! kotlinc writes a temporary as a local and then folds the ones whose value can stay on the
//! operand stack (`TemporaryVariablesEliminationTransformer`). krusty emitted the locals and never
//! folded them, so `println(f())` kept `astore; …; getstatic System.out; aload` where kotlinc has
//! `getstatic System.out; swap`. The class writer now runs the same rules over each method once its
//! tables are final.
//!
//! Before those rules kotlinc keeps a null-checked value on the stack for the path that loads it
//! again (`simplifyKnownSafeCallPatterns`): `aload x; ifnonnull L; …; L: aload x` becomes
//! `aload x; dup; ifnonnull L; pop; …; L:`, and the jump target's frame carries the value.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const SOURCE: &str = "fun label(n: Int): String = \"n=\" + n\n\
    fun printed(n: Int) {\n\
    \x20   println(label(n))\n\
    }\n\
    fun counted(n: Int) {\n\
    \x20   println(n + 1)\n\
    }\n";

#[test]
fn a_printed_value_stays_on_the_stack_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "TemporaryElimination",
        SOURCE,
        "TemporaryEliminationKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // `counted` folds too, but krusty spills an inline argument twice where kotlinc spills it once;
    // the folded copy's slot stays taken (kotlinc does not renumber), so its locals differ.
    let member = "void printed(int)";
    let reference = method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "{member} not found");
    assert_eq!(
        method_instructions(&built.krusty, member),
        reference,
        "{member}"
    );
}

#[test]
fn folded_temporaries_still_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   printed(3)\n\
             \x20   counted(4)\n\
             \x20   return if (label(5) == \"n=5\") \"OK\" else \"FAIL\"\n\
             }}\n"
        ),
        "folded temporaries",
    );
}

const NULL_CHECKS: &str = "fun orZero(s: String?): Int = if (s == null) 0 else s.length\n\
    fun measured(s: String?): Int {\n\
    \x20   val n = if (s == null) 0 else s.length\n\
    \x20   return n\n\
    }\n\
    fun required(s: String?): String {\n\
    \x20   if (s == null) throw Throwable()\n\
    \x20   return s\n\
    }\n\
    fun early(s: String?): Int {\n\
    \x20   if (s == null) return 0\n\
    \x20   return s.length\n\
    }\n";

/// The `StackMapTable` lines of `marker`'s method in a `javap -v` listing.
fn stack_map(disassembly: &str, marker: &str) -> Option<Vec<String>> {
    let mut lines = disassembly.lines().map(str::trim);
    lines
        .by_ref()
        .find(|line| line.ends_with(';') && line.contains(marker))?;
    // The next member's declaration ends this method's listing; a `descriptor:` line or a
    // commented constant (`// Method …:()Ljava/lang/String;`) has the same shape but belongs to it.
    let mut method = lines.take_while(|line| {
        !(line.ends_with(';')
            && line.contains('(')
            && !line.starts_with("descriptor:")
            && !line.contains("//"))
    });
    method
        .by_ref()
        .find(|line| line.starts_with("StackMapTable"))?;
    Some(
        method
            .take_while(|line| {
                line.starts_with("frame_type")
                    || line.starts_with("stack")
                    || line.starts_with("locals")
                    || line.starts_with("offset_delta")
            })
            .map(str::to_string)
            .collect(),
    )
}

#[test]
fn a_null_checked_value_stays_on_the_stack_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "NullCheckFold",
        NULL_CHECKS,
        "NullCheckFoldKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // `early` does not fold in kotlinc either: its `return 0` leaves a dead `nop` that falls into
    // the jump target, so the jump is not the target's only predecessor.
    for member in [
        "int orZero(java.lang.String)",
        "int measured(java.lang.String)",
        "java.lang.String required(java.lang.String)",
        "int early(java.lang.String)",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
        let frames = stack_map(&built.reference, member)
            .unwrap_or_else(|| panic!("{member} has no StackMapTable"));
        assert!(!frames.is_empty(), "{member} has no StackMapTable");
        assert_eq!(
            stack_map(&built.krusty, member),
            Some(frames),
            "{member} frames"
        );
    }
}

#[test]
fn folded_null_checks_still_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{NULL_CHECKS}\
             class Node(val name: String?, val next: Node?)\n\
             fun len(s: String?): Int? = s?.length\n\
             fun chain(b: Node?): String? = b?.next?.name\n\
             fun upper(s: String?): String = s?.uppercase() ?: \"none\"\n\
             fun orDefault(s: String?): String = s ?: \"d\"\n\
             fun printIt(s: String?) {{ println(s ?: \"x\") }}\n\
             fun box(): String {{\n\
             \x20   if (len(null) != null || len(\"ab\") != 2) return \"len\"\n\
             \x20   if (chain(null) != null || chain(Node(\"a\", null)) != null) return \"chain\"\n\
             \x20   if (chain(Node(\"a\", Node(\"b\", null))) != \"b\") return \"chain2\"\n\
             \x20   if (upper(null) != \"none\" || upper(\"a\") != \"A\") return \"upper\"\n\
             \x20   if (orDefault(null) != \"d\" || orDefault(\"q\") != \"q\") return \"orDefault\"\n\
             \x20   printIt(null)\n\
             \x20   printIt(\"p\")\n\
             \x20   if (orZero(null) != 0 || orZero(\"abc\") != 3) return \"orZero\"\n\
             \x20   if (measured(null) != 0 || measured(\"ab\") != 2) return \"measured\"\n\
             \x20   try {{\n\
             \x20       required(null)\n\
             \x20       return \"required\"\n\
             \x20   }} catch (e: Throwable) {{\n\
             \x20   }}\n\
             \x20   if (required(\"r\") != \"r\") return \"required2\"\n\
             \x20   if (early(null) != 0 || early(\"abcd\") != 4) return \"early\"\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "folded null checks",
    );
}
