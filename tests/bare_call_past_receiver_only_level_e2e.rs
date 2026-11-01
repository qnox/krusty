//! A bare call passes over a same-package extension, which needs a receiver, to the default
//! import of the same name. The program runs on the JVM against kotlinc; the KLIB lanes decline
//! the stdlib `check` body's exception, and the portable form is the box regression
//! `bareCallPastReceiverOnlyLevel.kt`.
use super::common;

const PROGRAM: &str =
    include_str!("fixtures/bare_call_past_receiver_only_level/BareCallPastReceiverOnlyLevel.kt");

#[test]
fn a_bare_call_reaches_the_default_import_past_a_same_package_extension() {
    common::expect_box_same_as_kotlinc(PROGRAM, "BareCallPastReceiverOnlyLevel");
}
