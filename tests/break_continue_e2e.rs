//! `break` / `continue` in `for` and `while` loops (including nested loops). The loop `update` (a
//! `for`-loop increment) runs at the `continue` target so `continue` advances rather than skipping it.
//! Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn break_continue_runs() {
    const SRC: &str = "fun box(): String {\n\
var s = 0\n\
for (i in 1..10) { if (i == 3) continue; if (i == 7) break; s += i }\n\
if (s != 1 + 2 + 4 + 5 + 6) return \"ffor\"\n\
var t = 0; var j = 0\n\
while (j < 10) { j += 1; if (j % 2 == 0) continue; if (j > 7) break; t += j }\n\
if (t != 1 + 3 + 5 + 7) return \"fwhile\"\n\
var u = 0\n\
for (a in 0 until 5) { for (b in 0 until 5) { if (b == 2) break; u += 1 } }\n\
if (u != 10) return \"fnest\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(SRC, "D");
}
