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
    fun size(s: String?): Int? = s?.length\n\
    fun sizeOr(s: String?): Int = s?.length ?: 0\n\
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
    }\n";

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
        "java.lang.Integer size(java.lang.String)",
        "int sizeOr(java.lang.String)",
        "java.lang.String nextName(Link)",
        "java.lang.String thirdName(Link)",
        "void touchIt(Link)",
        "void measureIt(java.lang.String)",
        "void growIt(Link)",
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
fn safe_calls_laid_out_like_kotlincs_still_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val chain = Link(\"a\", Link(\"b\", Link(\"c\", null)))\n\
             \x20   if (size(null) != null || size(\"ab\") != 2) return \"size\"\n\
             \x20   if (sizeOr(null) != 0 || sizeOr(\"abc\") != 3) return \"sizeOr\"\n\
             \x20   if (nextName(null) != null || nextName(Link(\"a\", null)) != null) return \"next\"\n\
             \x20   if (nextName(chain) != \"b\") return \"next2\"\n\
             \x20   if (thirdName(chain) != \"c\" || thirdName(Link(\"a\", Link(\"b\", null))) != null) return \"third\"\n\
             \x20   touchIt(null)\n\
             \x20   touchIt(chain)\n\
             \x20   measureIt(null)\n\
             \x20   measureIt(\"m\")\n\
             \x20   growIt(null)\n\
             \x20   growIt(chain)\n\
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
