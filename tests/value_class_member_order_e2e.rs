//! A value class's members in kotlinc's order and with its flags.
//!
//! kotlinc emits a value class's declared members first, then the `Any` overrides with their static
//! implementations (`toString-impl`, `toString`, `hashCode-impl`, `hashCode`, `equals-impl`,
//! `equals`), then the private primary `<init>` — `ACC_SYNTHETIC` as well — and finally the
//! generated representation members `constructor-impl`, `box-impl`, `unbox-impl`, `equals-impl0`.
//! krusty emitted `<init>` first and the representation members before the `Any` overrides, so no
//! value class matched, and it marked a secondary constructor's `constructor-impl` `final`.
use super::common;
use super::serialization_companion_byte_parity_e2e::compare_with_kotlinc_plugin;

/// The class's methods as `(access, name, descriptor)` in class-file order.
fn members(bytes: &[u8]) -> Vec<(u16, String, String)> {
    krusty::jvm::classreader::parse_class(bytes)
        .expect("a parseable class")
        .methods
        .iter()
        .map(|method| {
            (
                method.access,
                method.name.clone(),
                method.descriptor.clone(),
            )
        })
        .collect()
}

#[test]
fn a_value_class_orders_its_members_like_kotlinc() {
    for (tag, source, class) in [
        (
            "ValueClassOrderPlain",
            "@JvmInline value class N(val n: Int)\n",
            "N",
        ),
        (
            "ValueClassOrderMembers",
            "@JvmInline value class S(val s: String) {\n\
             \x20   init { require(s.isNotEmpty()) }\n\
             \x20   constructor(i: Int) : this(i.toString())\n\
             \x20   val len: Int get() = s.length\n\
             \x20   fun twice(): String = s + s\n\
             \x20   companion object { fun of(x: String) = S(x) }\n\
             }\n",
            "S",
        ),
    ] {
        let Some(built) =
            compare_with_kotlinc_plugin(tag, source, class, &[common::stdlib_jar()], "25", &[])
        else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        assert_eq!(
            members(&built.krusty_bytes),
            members(&built.reference_bytes),
            "{class}: member access, order and descriptors"
        );
    }
}

#[test]
fn a_reordered_value_class_still_runs() {
    common::expect_box_ok_with_stdlib(
        "@JvmInline value class S(val s: String) {\n\
         \x20   init { require(s.isNotEmpty()) }\n\
         \x20   constructor(i: Int) : this(i.toString())\n\
         \x20   val len: Int get() = s.length\n\
         \x20   companion object { fun of(x: String) = S(x) }\n\
         }\n\
         fun box(): String {\n\
         \x20   val boxed: Any = S(12)\n\
         \x20   if (boxed != S.of(\"12\") || boxed.hashCode() != \"12\".hashCode()) return \"FAIL \" + boxed\n\
         \x20   return if ((boxed as S).len == 2 && boxed.toString() == \"S(s=12)\") \"OK\" else \"FAIL \" + boxed\n\
         }\n",
        "a reordered value class",
    );
}
