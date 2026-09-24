//! A data class whose property is a NULLABLE value class held as its box.
//!
//! A nullable value class over a primitive (or a null-capable reference) underlying cannot carry
//! `null` unboxed, so the field holds the value-class box. The generated members used the
//! unboxed-carrier `-impl` functions on that box regardless:
//!
//! ```text
//! VerifyError: NullableTicketEnvelope.equals(Ljava/lang/Object;)Z: Type 'PrimitiveTicket'
//! (current frame, stack[1]) is not assignable to integer
//! ```
//!
//! kotlinc compares the two boxes with `Intrinsics.areEqual`, and hashes a non-null box through
//! `unbox-impl` then `hashCode-impl`.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const PRIMITIVE: &str = "@JvmInline\n\
    value class PrimitiveTicket(val value: Int)\n\
    data class NullableTicketEnvelope(val ticket: PrimitiveTicket?)\n";

const NULLABLE_REFERENCE: &str = "@JvmInline\n\
    value class NullableTextTicket(val value: String?)\n\
    data class NullableTicketEnvelope(val ticket: NullableTextTicket?)\n";

#[test]
fn a_boxed_nullable_value_class_property_drives_the_data_class_members() {
    for (tag, declarations, present) in [
        ("primitive", PRIMITIVE, "PrimitiveTicket(3)"),
        (
            "nullable reference",
            NULLABLE_REFERENCE,
            "NullableTextTicket(\"a\")",
        ),
    ] {
        let src = format!(
            "{declarations}\
             fun box(): String {{\n\
             \x20   val some = NullableTicketEnvelope({present})\n\
             \x20   val none = NullableTicketEnvelope(null)\n\
             \x20   if (some != NullableTicketEnvelope({present})) return \"FAIL equals\"\n\
             \x20   if (some == none) return \"FAIL not equals\"\n\
             \x20   if (some.hashCode() != {present}.hashCode()) return \"FAIL hashCode\"\n\
             \x20   if (none.hashCode() != 0) return \"FAIL null hashCode\"\n\
             \x20   if (none.toString() != \"NullableTicketEnvelope(ticket=null)\") return \"FAIL toString \" + none\n\
             \x20   return \"OK\"\n\
             }}\n"
        );
        common::expect_box_ok_with_stdlib(&src, &format!("a boxed nullable {tag} value class"));
    }
}

#[test]
fn a_boxed_nullable_value_class_member_matches_kotlinc() {
    for (tag, declarations) in [
        ("NullableValuePrimitive", PRIMITIVE),
        ("NullableValueReference", NULLABLE_REFERENCE),
    ] {
        let Some(built) = compare_with_kotlinc_plugin(
            tag,
            declarations,
            "NullableTicketEnvelope",
            &[common::stdlib_jar()],
            "25",
            &[],
        ) else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        for member in ["boolean equals(java.lang.Object)", "int hashCode()"] {
            assert_eq!(
                method_instructions(&built.krusty, member),
                method_instructions(&built.reference, member),
                "{tag}: {member}"
            );
        }
    }
}

#[test]
fn a_sibling_nested_nullable_chain_uses_its_terminal_carrier() {
    const PAYLOAD: &str = "package fixtures.nestednullable\n\
        @JvmInline\n\
        value class NullablePayload(val text: String?)\n";
    const TICKET: &str = "package fixtures.nestednullable\n\
        @JvmInline\n\
        value class SiblingTicket(val payload: NullablePayload)\n";
    const ENVELOPE: &str = "package fixtures.nestednullable\n\
        data class NestedNullableEnvelope(val ticket: SiblingTicket?)\n";
    const MAIN: &str = "package fixtures.nestednullable\n\
        fun box(): String {\n\
        \x20   val ticket = SiblingTicket(NullablePayload(\"payload\"))\n\
        \x20   val some = NestedNullableEnvelope(ticket)\n\
        \x20   val none = NestedNullableEnvelope(null)\n\
        \x20   if (some != NestedNullableEnvelope(ticket)) return \"FAIL equals\"\n\
        \x20   if (some == none) return \"FAIL not equals\"\n\
        \x20   if (some.hashCode() != ticket.hashCode()) return \"FAIL hashCode\"\n\
        \x20   if (none.hashCode() != 0) return \"FAIL null hashCode\"\n\
        \x20   return \"OK\"\n\
        }\n";
    let sources = [
        ("NullablePayload.kt", PAYLOAD),
        ("SiblingTicket.kt", TICKET),
        ("NestedNullableEnvelope.kt", ENVELOPE),
        ("Main.kt", MAIN),
    ];
    common::expect_box_ok_files_with_stdlib(
        &sources,
        "a sibling nested nullable value-class carrier",
    );
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "fixtures.nestednullable.MainKt"),
        "OK",
        "kotlinc reference for a sibling nested nullable value-class carrier",
    );
}
