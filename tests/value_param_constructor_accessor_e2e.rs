//! A constructor taking a value class is private, reached only through its synthetic
//! `(…, DefaultConstructorMarker)` accessor — from the class itself as well as from outside it.
//!
//! kotlinc leaves the accessor as the private primary's only caller: `copy`, the `$default`
//! overload, a secondary constructor's `this(…)`, and a member building another instance all go
//! through it. krusty called the private primary directly from inside the class, annotated the
//! private primary's parameters, and emitted the accessor beside the primary instead of after the
//! other members:
//!
//! ```text
//! kotlinc: private <init>(Integer, String)   … equals   public synthetic <init>(Integer, String, DefaultConstructorMarker)
//! krusty:  private <init>(Integer, String) @Nullable…   public synthetic <init>(…, DefaultConstructorMarker)   …
//! ```
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

const SOURCE: &str = "@JvmInline value class Tag(val raw: String)\n\
    data class Holder(val count: Int? = 0, val tag: Tag) {\n\
    \x20   constructor(count: Int) : this(count, Tag(\"s\"))\n\
    \x20   fun again() = Holder(tag = Tag(\"a\"))\n\
    \x20   companion object { fun make() = Holder(3) }\n\
    }\n";

const SECONDARY_DEFAULT_SOURCE: &str = "@JvmInline value class Token(val raw: Int)\n\
    class SecondaryDefault(val text: String) {\n\
    \x20   constructor(token: Token = Token(7)) : this(token.raw.toString())\n\
    }\n";

#[test]
fn every_construction_inside_the_class_runs() {
    let src = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   val holder = Holder(1)\n\
         \x20   if (holder != Holder(1, Tag(\"s\"))) return \"FAIL secondary \" + holder\n\
         \x20   if (holder.again() != Holder(0, Tag(\"a\"))) return \"FAIL default \" + holder.again()\n\
         \x20   if (holder.copy(count = 2).count != 2) return \"FAIL copy\"\n\
         \x20   if (Holder(tag = Tag(\"z\")).count != 0) return \"FAIL defaulted from outside\"\n\
         \x20   return if (Holder.make().count == 3) \"OK\" else \"FAIL companion \" + Holder.make()\n\
         }}\n"
    );
    common::expect_box_ok_with_stdlib(&src, "constructions inside a class hiding its primary");
}

#[test]
fn selected_secondary_super_constructor_is_not_treated_as_the_hidden_primary() {
    let src = "@JvmInline value class Tag(val raw: Int)\n\
        open class Base(val tag: Tag) {\n\
        \x20   constructor(value: Int, positive: Boolean) :\n\
        \x20       this(Tag(if (positive) value else -value))\n\
        }\n\
        class PrimaryChild : Base(3, true)\n\
        class SecondaryChild : Base {\n\
        \x20   constructor() : super(4, true)\n\
        }\n\
        fun box(): String {\n\
        \x20   if (PrimaryChild().tag.raw != 3) return \"FAIL primary child\"\n\
        \x20   if (SecondaryChild().tag.raw != 4) return \"FAIL secondary child\"\n\
        \x20   return \"OK\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "exact secondary superclass constructor selection");
}

#[test]
fn defaulted_secondary_with_a_value_class_parameter_runs() {
    let src = format!(
        "{SECONDARY_DEFAULT_SOURCE}\
         fun box(): String = if (SecondaryDefault().text == \"7\") \"OK\" else \"FAIL\"\n"
    );
    common::expect_box_ok_with_stdlib(&src, "defaulted hidden secondary constructor");
}

/// `Holder`'s methods as `(access, name, descriptor)` in class-file order.
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

/// The javap block of one member, from its header to the blank line after it.
fn member_block<'a>(disassembly: &'a str, header: &str) -> Vec<&'a str> {
    disassembly
        .lines()
        .skip_while(|line| line.trim() != header)
        .take_while(|line| !line.trim().is_empty())
        .collect()
}

#[test]
fn the_class_matches_kotlinc_around_its_hidden_primary() {
    let Some(built) = compare_with_kotlinc_plugin(
        "HiddenPrimaryAccessor",
        SOURCE,
        "Holder",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_eq!(
        members(&built.krusty_bytes),
        members(&built.reference_bytes),
        "member access, order and descriptors"
    );
    let primary = "private Holder(java.lang.Integer, java.lang.String);";
    let reference_primary = member_block(&built.reference, primary);
    assert!(!reference_primary.is_empty(), "{}", built.reference);
    assert!(
        !member_block(&built.krusty, primary)
            .iter()
            .any(|line| line.contains("RuntimeInvisibleParameterAnnotations")),
        "the private primary carries no parameter nullability"
    );
    // Instructions only: a member without a `LocalVariableTable` lets the reader run on into its
    // parameter-annotation entries (`0: #`), which are compared separately above.
    let instructions = |disassembly: &str, member: &str| {
        method_instructions(disassembly, member)
            .into_iter()
            .filter(|row| !row.ends_with(": #"))
            .collect::<Vec<_>>()
    };
    for member in [
        "Holder(int)",
        "Holder(java.lang.Integer, java.lang.String, int, kotlin.jvm.internal.DefaultConstructorMarker)",
        "Holder again()",
        "Holder copy-",
    ] {
        assert_eq!(
            instructions(&built.krusty, member),
            instructions(&built.reference, member),
            "{member}"
        );
    }
}

#[test]
fn default_stub_targets_the_selected_hidden_secondary_accessor() {
    let Some(built) = compare_with_kotlinc_plugin(
        "HiddenSecondaryDefaultAccessor",
        SECONDARY_DEFAULT_SOURCE,
        "SecondaryDefault",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_eq!(
        members(&built.krusty_bytes),
        members(&built.reference_bytes),
        "member access, order and descriptors"
    );
    for member in [
        "SecondaryDefault(int, kotlin.jvm.internal.DefaultConstructorMarker)",
        "SecondaryDefault(int, int, kotlin.jvm.internal.DefaultConstructorMarker)",
    ] {
        assert_eq!(
            method_instructions(&built.krusty, member),
            method_instructions(&built.reference, member),
            "{member}"
        );
    }
}
