//! A value-class property read handed on as a constructor argument or boxed into `Any`.
//!
//! The read keeps its logical value-class type in the IR while its accessor returns the carrier
//! (`getTag-…()Ljava/lang/String;`). Emission typed the stack from the logical type, took the carrier
//! for the value-class box and "unboxed" it again:
//!
//! ```text
//! VerifyError: Holder.twin()LHolder; @12: invokevirtual
//!   Type 'java/lang/String' (current frame, stack[3]) is not assignable to 'Tag'
//! ```
//!
//! It now types the read from the carrier the value-class pass records beside it.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "@JvmInline value class Tag(val raw: String)\n\
    class Holder(val count: Int, val tag: Tag) {\n\
    \x20   fun twin() = Holder(count, tag)\n\
    \x20   fun other(holder: Holder) = Holder(count, holder.tag)\n\
    \x20   fun pass() = Other(tag)\n\
    \x20   fun passFrom(holder: Holder) = Other(holder.tag)\n\
    }\n\
    class Other(val tag: Tag)\n";

#[test]
fn a_value_class_property_is_passed_on_as_its_carrier() {
    let src = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   val holder = Holder(1, Tag(\"a\"))\n\
         \x20   if (holder.twin().tag.raw != \"a\") return \"FAIL twin\"\n\
         \x20   if (holder.other(Holder(2, Tag(\"b\"))).tag.raw != \"b\") return \"FAIL other\"\n\
         \x20   if (Other(holder.tag).tag.raw != \"a\") return \"FAIL constructor\"\n\
         \x20   if (holder.pass().tag.raw != \"a\") return \"FAIL pass\"\n\
         \x20   val boxed: Any = holder.tag\n\
         \x20   return if (boxed == Tag(\"a\")) \"OK\" else \"FAIL box \" + boxed\n\
         }}\n"
    );
    common::expect_box_ok_with_stdlib(&src, "a value-class property passed on");
}

#[test]
fn a_value_class_property_argument_matches_kotlinc() {
    let Some(built) = compare_with_kotlinc_plugin(
        "ValueClassPropertyArgument",
        SOURCE,
        "Holder",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // `twin`/`other` construct their own class, which kotlinc routes through the accessor; this
    // compares the carrier hand-off where both compilers already agree on the target.
    for member in ["Other pass()", "Other passFrom(Holder)"] {
        assert_eq!(
            method_instructions(&built.krusty, member),
            method_instructions(&built.reference, member),
            "{member}"
        );
    }
}
