//! A generic bridge over `X?`, where `X` wraps a non-null reference, passes null through both of
//! its adapters as kotlinc does: the argument unboxes past `unbox-impl` only when it is not null,
//! and the result boxes through `box-impl` only when it is not null. The override takes and
//! returns `X`'s carrier, so the bridge also calls it by the carrier descriptor. A nullable class
//! over a primitive is boxed on both sides and needs no adapter. A non-null `X` whose own carrier
//! may be null (`X(val any: Any?)`) boxes it whatever it holds: `X(null)` is a value, not null.

use super::common;

const SRC: &str = "@JvmInline value class Tag(val name: String)\n\
    @JvmInline value class Num(val n: Int)\n\
    interface Echo<T> { fun echo(t: T): T }\n\
    class MaybeTag : Echo<Tag?> { override fun echo(t: Tag?): Tag? = t }\n\
    class MaybeNum : Echo<Num?> { override fun echo(t: Num?): Num? = t }\n\
    @JvmInline value class Loose(val any: Any?)\n\
    interface Source { fun get(): Loose? }\n\
    class Present : Source { override fun get(): Loose = Loose(null) }\n\
    fun box(): String {\n\
    \x20   val tags: Echo<Tag?> = MaybeTag()\n\
    \x20   if (tags.echo(null) != null) return \"null tag\"\n\
    \x20   if (tags.echo(Tag(\"O\"))?.name != \"O\") return \"tag\"\n\
    \x20   val nums: Echo<Num?> = MaybeNum()\n\
    \x20   if (nums.echo(null) != null) return \"null num\"\n\
    \x20   if (nums.echo(Num(1))?.n != 1) return \"num\"\n\
    \x20   val source: Source = Present()\n\
    \x20   if (source.get() != Loose(null)) return \"loose\"\n\
    \x20   return \"OK\"\n\
    }\n";

const ECHO_BRIDGE: &str = "public java.lang.Object echo(java.lang.Object);";

fn assert_bridge_matches(class: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "ValueClassNullableBridge",
        SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits {class}.{member}");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference,
        "{class}.{member}"
    );
}

#[test]
fn a_bridge_over_a_nullable_reference_carrier_keeps_null_like_kotlinc() {
    assert_bridge_matches("MaybeTag", ECHO_BRIDGE);
}

#[test]
fn a_bridge_over_a_nullable_boxed_value_class_passes_the_box_like_kotlinc() {
    assert_bridge_matches("MaybeNum", ECHO_BRIDGE);
}

#[test]
fn a_bridge_boxes_a_non_null_class_over_a_nullable_carrier_like_kotlinc() {
    assert_bridge_matches("Present", "public Loose get-Xwt3j-o();");
}

#[test]
fn nullable_value_class_bridges_run() {
    common::expect_box_ok_with_stdlib(SRC, "ValueClassNullableBridge");
}
