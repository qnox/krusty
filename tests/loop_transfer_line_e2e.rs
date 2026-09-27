//! kotlinc's `visitBreakContinue` marks a `break`/`continue`'s line and emits a `nop` before the
//! jump. `RedundantNopsCleanup` drops the `nop` when the jump shares its line, and `NegatedJumps`
//! then folds a guard written on the same line into one inverted jump; a transfer on a line of its
//! own keeps that line on its `goto`, which leaves the guard as a branch around it.
use super::common;

#[test]
fn loop_transfers_are_byte_identical_to_kotlinc() {
    let src = "package store\n\
               \n\
               fun sink(x: Int) {}\n\
               \n\
               fun guards(a: Int) {\n\
               \x20   for (i in 0 until a) {\n\
               \x20       if (i == 3)\n\
               \x20           continue\n\
               \x20       if (i == 5) break\n\
               \x20       if (i == 7) {\n\
               \x20           break\n\
               \x20       }\n\
               \x20       sink(i)\n\
               \x20   }\n\
               }\n\
               \n\
               fun labeled(a: Int) {\n\
               \x20   outer@ for (i in 0 until a) {\n\
               \x20       for (j in 0 until a) {\n\
               \x20           if (j == i)\n\
               \x20               continue@outer\n\
               \x20           if (j > 4) break@outer\n\
               \x20           sink(j)\n\
               \x20       }\n\
               \x20   }\n\
               }\n";
    common::byte_diff_against_kotlinc("LoopTransferLine", src, "store/LoopTransferLineKt")
        .expect("reference kotlinc is provisioned")
        .expect("store/LoopTransferLineKt byte-identical to kotlinc");
}
