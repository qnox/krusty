//! A bound reference captures a value-class receiver as its box, because the reference class's
//! constructor declares that receiver as `Any`. Over an `Any?` carrier the box and the carrier
//! share the JVM type `Object`, and krusty passed the carrier unboxed, which the reference's
//! `invoke` then failed to cast to the value class.

use super::common;

const SRC: &str = "@JvmInline value class Tag(val a: Any?) {\n\
    \x20   fun show(): String = if (a == null) \"null\" else \"tag\"\n\
    }\n\
    fun bind(t: Tag): () -> String = t::show\n";

#[test]
fn a_bound_receiver_over_an_any_carrier_is_boxed() {
    let built = common::compare_with_kotlinc_plugin(
        "BoundReferenceReceiver",
        SRC,
        "BoundReferenceReceiverKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "bind-";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes bind");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_bound_receiver_over_an_any_carrier_runs() {
    let src =
        format!("{SRC}fun box(): String = if (bind(Tag(1))() == \"tag\") \"OK\" else \"fail\"\n");
    assert_eq!(common::expect_box_run_with_stdlib(&src, "Main"), "OK");
}
