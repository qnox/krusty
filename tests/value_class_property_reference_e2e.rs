//! A property reference whose RECEIVER is a value class.
//!
//! A value class's members are realized statically over its erased carrier — `Z.getXx-impl(I)I`,
//! and `ExtKt.getXx-IQRRRT4(I)I` for an extension on one. The property-reference class named an
//! ordinary instance accessor instead (`Z.getXx()I`), which is declared nowhere, so every one of
//! these programs failed at its first `get` with a `NoSuchMethodError` — an artifact that could not
//! link, emitted without a diagnostic.
//!
//! A second defect sat beside it: a property whose TYPE is a value class already had its accessor
//! NAME mangled, but a member or top-level property has no written descriptor, so the one the
//! emitter synthesized from the property's semantic type (`()LZ;`) named a method the declaration
//! does not have either — it returns the carrier, `()I`.
//!
//! Each case here is a corpus shape (`codegen/box/inlineClasses/callableReferences/`) reduced to
//! its smallest form, and every emitted body was compared against the reference compiler's.

use super::common;

fn run(source: &str, stem: &str) -> String {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::compile_and_run_box(source, stem, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{stem} must compile and run"))
}

/// A MEMBER of a value class: the accessor is `Z.getXx-impl(I)I`, so the reference unboxes its
/// receiver and calls it statically — byte-identical to the reference compiler's
/// `checkcast Z; unbox-impl; invokestatic getXx-impl`.
#[test]
fn a_reference_to_a_member_of_a_value_class_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int) {\n\
             \x20   val xx get() = x\n\
             }\n\
             \n\
             @JvmInline\n\
             value class S(val x: String) {\n\
             \x20   val xx get() = x\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   if ((Z::xx).get(Z(42)) != 42) return \"FAIL1\"\n\
             \x20   if ((S::xx).get(S(\"ab\")) != \"ab\") return \"FAIL2\"\n\
             \x20   if (Z(7)::xx.get() != 7) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassMemberRef",
        ),
        "OK"
    );
}

/// An EXTENSION on a value class: the accessor is the facade's hash-mangled
/// `getXx-IQRRRT4(I)I`, not `getXx(LZ;)I`. Its receiver is unboxed the same way.
#[test]
fn a_reference_to_an_extension_on_a_value_class_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int)\n\
             \n\
             val Z.xx get() = x + 1\n\
             \n\
             fun box(): String {\n\
             \x20   if ((Z::xx).get(Z(41)) != 42) return \"FAIL1\"\n\
             \x20   if (Z(1)::xx.get() != 2) return \"FAIL2\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassExtensionRef",
        ),
        "OK"
    );
}

/// The value class's own UNDERLYING property is the exception: reading it is the unbox, and its
/// accessor stays an ordinary instance getter on the box (`Z.getX()I`). A reference to it must not
/// be rewritten — doing so named `Z.getX-impl(I)I`, which does not exist.
#[test]
fn a_reference_to_a_value_classs_underlying_property_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int)\n\
             \n\
             @JvmInline\n\
             value class S(val x: String)\n\
             \n\
             fun box(): String {\n\
             \x20   if ((Z::x).get(Z(42)) != 42) return \"FAIL1\"\n\
             \x20   if ((S::x).get(S(\"ab\")) != \"ab\") return \"FAIL2\"\n\
             \x20   if (Z(7)::x.get() != 7) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassUnderlyingRef",
        ),
        "OK"
    );
}

/// A property whose TYPE is a value class: its accessor exchanges the CARRIER (`C.getZ-a_XrcN0()I`,
/// `C.setZ-IQRRRT4(I)V`), which the synthesized descriptor has to say — it used to claim `()LZ;`.
#[test]
fn a_reference_to_a_value_class_typed_member_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int)\n\
             \n\
             class C(var z: Z)\n\
             \n\
             fun box(): String {\n\
             \x20   val ref = C::z\n\
             \x20   val c = C(Z(42))\n\
             \x20   if (ref.get(c).x != 42) return \"FAIL1\"\n\
             \x20   ref.set(c, Z(1234))\n\
             \x20   if (ref.get(c).x != 1234) return \"FAIL2\"\n\
             \x20   if (c::z.get().x != 1234) return \"FAIL3\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassTypedMemberRef",
        ),
        "OK"
    );
}

/// A TOP-LEVEL property of value-class type stays on the BOXED convention here, because its
/// backing field and accessors do (unlike the reference compiler's, which erase them — a separate
/// declaration-side item). Mangling the reference's accessor while the declaration kept its plain
/// name is what made this shape unlinkable, so the reference must stay on the same convention as
/// the declaration it calls.
#[test]
fn a_reference_to_a_value_class_typed_top_level_property_resolves() {
    assert_eq!(
        run(
            "@JvmInline\n\
             value class Z(val x: Int)\n\
             \n\
             var topLevel: Z = Z(0)\n\
             val readOnly: Z = Z(9)\n\
             \n\
             fun box(): String {\n\
             \x20   val ref = ::topLevel\n\
             \x20   ref.set(Z(42))\n\
             \x20   if (ref.get().x != 42) return \"FAIL1\"\n\
             \x20   if ((::readOnly).get().x != 9) return \"FAIL2\"\n\
             \x20   return \"OK\"\n\
             }\n",
            "ValueClassTopLevelRef",
        ),
        "OK"
    );
}
