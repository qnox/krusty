//! Operands already on the stack stay there while a later operand branches, as kotlinc keeps them:
//! only a handler, a suspension, or a loop transfer in the later operand moves them to locals.
use super::common;

const SOURCE: &str = "class Operands(val a: Int, val b: Int)\n\
                      class Holder { var f: Int = 0 }\n\
                      fun take(a: Int, b: Int) = a + b\n\
                      fun call(s: String?) = take(1, s?.length ?: 0)\n\
                      fun construct(s: String?) = Operands(2, s?.length ?: 0)\n\
                      fun concat(s: String?, x: Int) = \"a\" + x + (s?.length ?: 0)\n\
                      fun store(s: String?, a: IntArray) { a[0] = s?.length ?: 0 }\n\
                      fun set(s: String?, h: Holder) { h.f = s?.length ?: 0 }\n\
                      fun wide(s: String?, x: Long) = x + (s?.length ?: 0)\n\
                      fun invoke(s: String?, f: (Int, Int) -> Int) = f(3, s?.length ?: 0)\n\
                      fun branch(c: Boolean) = take(4, if (c) 5 else 6)\n";

#[test]
fn an_operand_prefix_stays_on_the_stack_across_a_branch_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "LiveOperandPrefix",
        SOURCE,
        "LiveOperandPrefixKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("LiveOperandPrefixKt differs from kotlinc:\n{diff}"));
}

#[test]
fn a_live_operand_prefix_keeps_its_values() {
    let src = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   if (call(\"ab\") != 3 || call(null) != 1) return \"call\"\n\
         \x20   val p = construct(\"abc\")\n\
         \x20   if (p.a != 2 || p.b != 3) return \"construct\"\n\
         \x20   if (concat(null, 5) != \"a50\") return \"concat\"\n\
         \x20   val a = IntArray(1)\n\
         \x20   store(\"xy\", a)\n\
         \x20   if (a[0] != 2) return \"store\"\n\
         \x20   val h = Holder()\n\
         \x20   set(\"xyz\", h)\n\
         \x20   if (h.f != 3) return \"set\"\n\
         \x20   if (wide(\"x\", 10L) != 11L) return \"wide\"\n\
         \x20   if (invoke(null) {{ l, r -> l * 10 + r }} != 30) return \"invoke\"\n\
         \x20   if (branch(false) != 10) return \"branch\"\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    let actual =
        common::compile_and_run_box(&src, "live_operand_prefix", &[common::stdlib_jar()], None)
            .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}
