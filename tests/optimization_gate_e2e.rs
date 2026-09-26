//! kotlinc's `canBeOptimized` gate: its optimization passes run only over a method whose analysis
//! frames would weigh less than 50 MiB (frames times `max_locals + max_stack`). A larger method
//! gets only the final dead-code step, so a condition on a local holding a constant, which the
//! constant-condition pass folds in a small method, keeps its jump there.
//!
//! krusty ran every pass over every method, so it folded the jump kotlinc keeps.
use super::common;

/// The condition the constant-condition pass folds: `k` holds 2, so `k > 1` always holds.
const FOLDED: &str = "    val k = 2\n    return if (k > 1) h() else 0\n";

/// A function of `longs` unused `Long` locals (two slots each) followed by [`FOLDED`].
fn function(name: &str, longs: usize) -> String {
    let mut body = format!("fun {name}(): Int {{\n");
    for index in 0..longs {
        body.push_str(&format!("    val a{index} = 0L\n"));
    }
    body.push_str(FOLDED);
    body.push_str("}\n");
    body
}

/// `small`, `below` and `above` compiled by kotlinc and krusty, which must be the same bytes.
///
/// Each `Long` local is two instructions (`lconst_0; lstore`), which are kotlinc's frames, and a
/// label and a line number besides, which krusty's analyzer keeps a frame for too; a method is
/// `2n + 3` values wide. `below` (2,500 locals) weighs about 24 MiB by kotlinc's count and 48 by
/// krusty's, so both optimize it. `above` (3,700 locals) weighs over 52 MiB by kotlinc's count,
/// so both leave its jump.
#[test]
fn a_method_past_the_memory_limit_is_not_optimized_like_kotlincs() {
    let source = format!(
        "package gate\n\nfun h(): Int = 1\n\n{}\n{}\n{}",
        function("small", 0),
        function("below", 2_500),
        function("above", 3_700),
    );
    common::byte_diff_against_kotlinc("OptimizationGate", &source, "gate/OptimizationGateKt")
        .expect("the reference kotlinc is required")
        .unwrap_or_else(|difference| panic!("{difference}"));
}
