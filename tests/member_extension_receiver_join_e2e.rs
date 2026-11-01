//! A member extension joins its covariant receiver with its arguments.
//!
//! This is the complete program, run against kotlinc: an implicit receiver from `with`, and a
//! contravariant `(T) -> Unit` argument that leaves the receiver's type in place. The Wasm and
//! Native KLIB lanes do not yet realize the stdlib `with` body or lower the same-module `consume`
//! extension that takes a function-typed parameter, so the box regression
//! `memberExtensionCovariantReceiverJoin.kt` repeats the inference over local declarations on every
//! target.
use super::common;

const PROGRAM: &str =
    include_str!("fixtures/member_extension_receiver_join/MemberExtensionCovariantReceiverJoin.kt");

#[test]
fn member_extension_receiver_joins_like_kotlinc() {
    common::expect_box_same_as_kotlinc(PROGRAM, "MemberExtensionCovariantReceiverJoin");
}
