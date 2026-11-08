//! Inference over a type parameter whose upper bound names itself (`out T : Out<T>`). Solving the
//! call `o.copy(x)` on `Out<Out<*>>` binds `T` to a type that names `T` again; applying that binding
//! must expand it once, as kotlinc does, rather than re-substitute it forever. The program is also a
//! box regression, so every backend lane runs it.

use super::common;

#[test]
fn a_call_on_a_self_bounded_receiver_finishes_and_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/selfBoundedReceiverCopy.kt"),
        "SelfBoundedCopy",
    );
}
