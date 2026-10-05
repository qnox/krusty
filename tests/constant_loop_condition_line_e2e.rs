//! kotlinc's `visitConst` marks a Boolean constant's line on a `nop`: the constant need not be
//! materialized, and a debugger still has to stop on its line. A loop over a constant condition is
//! decided statically, so `while (true)` keeps its header line, and `do … while (false)` and
//! `do … while (true)` keep their bottom line, only through that `nop`.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      class Sink {\n\
                      \x20   var n = 0\n\
                      \x20   fun add(x: Int) { n += x }\n\
                      }\n\
                      \n\
                      fun header(s: Sink): Int {\n\
                      \x20   while (true) {\n\
                      \x20       s.add(1)\n\
                      \x20       break\n\
                      \x20   }\n\
                      \x20   return s.n\n\
                      }\n\
                      \n\
                      fun nested(s: Sink): Int {\n\
                      \x20   outer@ while (true) {\n\
                      \x20       while (true) {\n\
                      \x20           s.add(1)\n\
                      \x20           if (s.n > 2) break@outer\n\
                      \x20       }\n\
                      \x20   }\n\
                      \x20   return s.n\n\
                      }\n\
                      \n\
                      fun once(s: Sink): Int {\n\
                      \x20   do {\n\
                      \x20       s.add(1)\n\
                      \x20   } while (false)\n\
                      \x20   return s.n\n\
                      }\n\
                      \n\
                      fun forever(s: Sink): Int {\n\
                      \x20   do {\n\
                      \x20       s.add(1)\n\
                      \x20       if (s.n > 3) break\n\
                      \x20   } while (true)\n\
                      \x20   return s.n\n\
                      }\n\
                      \n\
                      fun never(s: Sink): Int {\n\
                      \x20   while (false) {\n\
                      \x20       s.add(1)\n\
                      \x20   }\n\
                      \x20   return s.n\n\
                      }\n";

#[test]
fn constant_loop_conditions_are_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "ConstantLoopCondition",
        SOURCE,
        "store/ConstantLoopConditionKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/ConstantLoopConditionKt differs from kotlinc: {diff}"));
}
