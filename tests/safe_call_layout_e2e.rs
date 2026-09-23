//! Safe calls laid out as kotlinc writes them.
//!
//! kotlinc checks a safe call's receiver with `ifnull` to the null path and lets the selector fall
//! through; a chain `a?.b?.c` has one null path that every guard jumps to, and a safe call whose
//! value is unused discards inside its guard. Its bytecode pass then keeps each checked value on
//! the stack. krusty's lowering records every safe call's guard, and the JVM emitter lays it out
//! the same way.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};
use super::temporary_elimination_e2e::stack_map;

const SOURCE: &str = "class Link(val name: String?, val next: Link?) {\n\
    \x20   fun touch() {}\n\
    \x20   fun grow(): Link = Link(name, this)\n\
    }\n\
    fun nullableLength(s: String?): Int? = s?.length\n\
    fun lengthOrZero(s: String?): Int = s?.length ?: 0\n\
    fun nextName(link: Link?): String? = link?.next?.name\n\
    fun thirdName(link: Link?): String? = link?.next?.next?.name\n\
    fun touchIt(link: Link?) {\n\
    \x20   link?.touch()\n\
    }\n\
    fun measureIt(s: String?) {\n\
    \x20   s?.length\n\
    }\n\
    fun growIt(link: Link?) {\n\
    \x20   link?.grow()\n\
    }\n\
    class Token {\n\
    \x20   fun stamp(): String = \"seen\"\n\
    }\n\
    fun boundary(token: Token?): String {\n\
    \x20   val scoped = token\n\
    \x20   scoped?.stamp()\n\
    \x20   return \"OK\"\n\
    }\n";

/// One method's complete offset-bearing rows from a debug table, in emitted order.
fn method_debug_table(text: &str, marker: &str, table: &str) -> Vec<String> {
    let mut lines = text.lines().map(str::trim);
    lines
        .by_ref()
        .find(|line| line.ends_with(';') && line.contains(marker))
        .unwrap_or_else(|| panic!("{marker} not found"));
    let mut method = lines.take_while(|line| {
        !(line.ends_with(';')
            && line.contains('(')
            && !line.starts_with("descriptor:")
            && !line.contains("//"))
    });
    method
        .by_ref()
        .find(|line| line.starts_with(table))
        .unwrap_or_else(|| panic!("{marker} has no {table}"));
    method
        .skip_while(|line| line.starts_with("Start "))
        .take_while(|line| {
            line.starts_with("line ")
                || line.starts_with(|character: char| character.is_ascii_digit())
        })
        .map(str::to_string)
        .collect()
}

#[test]
fn safe_calls_are_laid_out_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "SafeCallLayout",
        SOURCE,
        "SafeCallLayoutKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in [
        "java.lang.Integer nullableLength(java.lang.String)",
        "int lengthOrZero(java.lang.String)",
        "java.lang.String nextName(Link)",
        "java.lang.String thirdName(Link)",
        "void touchIt(Link)",
        "void measureIt(java.lang.String)",
        "void growIt(Link)",
        "java.lang.String boundary(Token)",
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

    let member = "java.lang.String boundary(Token)";
    for table in ["LineNumberTable", "LocalVariableTable"] {
        let reference = method_debug_table(&built.reference, member, table);
        assert!(!reference.is_empty(), "{member} has an empty {table}");
        assert_eq!(
            method_debug_table(&built.krusty, member, table),
            reference,
            "{member} {table}"
        );
    }
}

#[test]
fn safe_calls_laid_out_like_kotlincs_still_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val chain = Link(\"a\", Link(\"b\", Link(\"c\", null)))\n\
             \x20   if (nullableLength(null) != null || nullableLength(\"ab\") != 2) return \"nullableLength\"\n\
             \x20   if (lengthOrZero(null) != 0 || lengthOrZero(\"abc\") != 3) return \"lengthOrZero\"\n\
             \x20   if (nextName(null) != null || nextName(Link(\"a\", null)) != null) return \"next\"\n\
             \x20   if (nextName(chain) != \"b\") return \"next2\"\n\
             \x20   if (thirdName(chain) != \"c\" || thirdName(Link(\"a\", Link(\"b\", null))) != null) return \"third\"\n\
             \x20   touchIt(null)\n\
             \x20   touchIt(chain)\n\
             \x20   measureIt(null)\n\
             \x20   measureIt(\"m\")\n\
             \x20   growIt(null)\n\
             \x20   growIt(chain)\n\
             \x20   if (boundary(null) != \"OK\" || boundary(Token()) != \"OK\") return \"boundary\"\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "safe calls laid out like kotlinc's",
    );
}

#[test]
fn a_chain_across_lines_types_its_shared_null_path() {
    // Written across lines, the chain keeps its receiver temporaries (a line number between the
    // guards stops the bytecode fold), so the emitted frames are what the verifier checks. The
    // guards past the first jump to the shared null path before their own temporary is stored:
    // there, and at the join after it, those temporaries are unassigned.
    common::expect_box_ok_with_stdlib(
        "class Link(val name: String?, val next: Link?) {\n\
         \x20   fun find(key: String): Link? = if (key == name) this else next\n\
         }\n\
         val Link.onward: Link? get() = next\n\
         fun parse(s: String): Link = Link(s, Link(\"b\", Link(\"c\", null)))\n\
         class Holder {\n\
         \x20   private fun pick(response: String): String? =\n\
         \x20       runCatching {\n\
         \x20           parse(response)\n\
         \x20               .find(\"a\")\n\
         \x20               ?.onward\n\
         \x20               ?.find(\"b\")\n\
         \x20               ?.onward\n\
         \x20               ?.name\n\
         \x20       }.getOrNull()\n\
         \x20   fun call(response: String) = pick(response)\n\
         }\n\
         fun box(): String {\n\
         \x20   val holder = Holder()\n\
         \x20   if (holder.call(\"x\") != null) return \"miss\"\n\
         \x20   return if (holder.call(\"a\") == \"c\") \"OK\" else \"hit\"\n\
         }\n",
        "safe-call chain across lines",
    );
}

const ELVIS_OVER_CHAINS: &str = "class Link(val name: String?, val next: Link?) {\n\
    \x20   fun markerScore(): Int = 1\n\
    }\n\
    fun nameOr(link: Link?): String = link?.name ?: \"x\"\n\
    fun nextNameOr(link: Link?): String = link?.next?.name ?: \"x\"\n\
    fun thirdNameOr(link: Link?): String = link?.next?.next?.name ?: \"x\"\n\
    fun nextScoreOr(link: Link?): Int = link?.next?.markerScore() ?: 0\n\
    fun valueOr(s: String?): String = s ?: \"d\"\n\
    fun printNextName(link: Link?) {\n\
    \x20   println(link?.next?.name ?: \"x\")\n\
    }\n\
    fun discardNextName(link: Link?) {\n\
    \x20   link?.next?.name ?: \"x\"\n\
    }\n";

#[test]
fn an_elvis_over_a_safe_call_chain_joins_its_null_path_like_kotlincs() {
    // Every guard of the chain, and the elvis's own check of the chain's value, jump to the elvis's
    // right side; kotlinc's last pass turns the final `ifnull N; goto E; N:` into `ifnonnull E`.
    let Some(built) = compare_with_kotlinc_plugin(
        "ElvisChain",
        ELVIS_OVER_CHAINS,
        "ElvisChainKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in [
        "java.lang.String nameOr(Link)",
        "java.lang.String nextNameOr(Link)",
        "java.lang.String thirdNameOr(Link)",
        "int nextScoreOr(Link)",
        "java.lang.String valueOr(java.lang.String)",
        "void printNextName(Link)",
        "void discardNextName(Link)",
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
fn an_elvis_over_a_safe_call_chain_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{ELVIS_OVER_CHAINS}\
             fun box(): String {{\n\
             \x20   val chain = Link(\"a\", Link(null, Link(\"c\", null)))\n\
             \x20   if (nameOr(null) != \"x\" || nameOr(chain) != \"a\") return \"one\"\n\
             \x20   if (nextNameOr(chain) != \"x\" || nextNameOr(Link(null, null)) != \"x\") return \"two\"\n\
             \x20   if (thirdNameOr(chain) != \"c\" || thirdNameOr(null) != \"x\") return \"three\"\n\
             \x20   if (nextScoreOr(chain) != 1 || nextScoreOr(Link(null, null)) != 0) return \"score\"\n\
             \x20   if (valueOr(null) != \"d\" || valueOr(\"v\") != \"v\") return \"plain\"\n\
             \x20   printNextName(chain)\n\
             \x20   printNextName(null)\n\
             \x20   discardNextName(chain)\n\
             \x20   discardNextName(null)\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "elvis over a safe-call chain",
    );
}
