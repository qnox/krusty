//! A top-level `lateinit var` throws `UninitializedPropertyAccessException` until it is assigned.
//!
//! A public or internal one is a public static field whose getter throws. A private one keeps a
//! private field; the same file inlines the guard, and another class does so after `access$get…$p`.
//! `::prop.isInitialized` reads the field without that guard.

use super::common::{
    self, compare_with_kotlinc_plugin, expect_box_same_as_kotlinc, member_table,
    method_instructions,
};

const PAYLOAD: &str = "class Payload(val token: String)\n";
const THROW: &str = "@file:Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\nimport kotlin.UninitializedPropertyAccessException\n";

const SHAPES: &str = r#"
class Payload(val token: String)

lateinit var item: Payload
private lateinit var hidden: Payload
internal lateinit var inside: Payload

fun readHidden(): Payload = hidden
fun ready(): Boolean = ::item.isInitialized
fun hiddenReady(): Boolean = ::hidden.isInitialized
fun box(): String = item.token
fun readInside(): Payload = inside

object C {
    fun getS(): Payload = hidden
    fun ready(): Boolean = ::hidden.isInitialized
}
"#;

#[test]
fn an_uninitialized_top_level_read_throws() {
    expect_box_same_as_kotlinc(
        &format!(
            r#"
{THROW}
{PAYLOAD}
lateinit var item: Payload

fun box(): String {{
    try {{
        val seen = item
        return "Should throw an exception, got ${{seen.token}}"
    }} catch (e: UninitializedPropertyAccessException) {{
        return "OK"
    }} catch (e: Throwable) {{
        return "Unexpected exception: ${{e::class}}"
    }}
}}
"#
        ),
        "TopLevelLateinitRead",
    );
}

#[test]
fn an_uninitialized_top_level_member_access_throws() {
    expect_box_same_as_kotlinc(
        &format!(
            r#"
{THROW}
{PAYLOAD}
lateinit var item: Payload

fun box(): String {{
    try {{
        val token = item.token
        return "Should throw an exception, got $token"
    }} catch (e: UninitializedPropertyAccessException) {{
        return "OK"
    }} catch (e: Throwable) {{
        return "Unexpected exception: ${{e::class}}"
    }}
}}
"#
        ),
        "TopLevelLateinitMember",
    );
}

#[test]
fn a_private_top_level_read_from_another_class_throws() {
    expect_box_same_as_kotlinc(
        &format!(
            r#"
{THROW}
{PAYLOAD}
private lateinit var item: Payload

object C {{
    fun getItem() = item
}}

fun box(): String {{
    try {{
        val seen = C.getItem()
        return "Should throw an exception, got ${{seen.token}}"
    }} catch (e: UninitializedPropertyAccessException) {{
        return "OK"
    }} catch (e: Throwable) {{
        return "Unexpected exception: ${{e::class}}"
    }}
}}
"#
        ),
        "TopLevelLateinitBridge",
    );
}

#[test]
fn an_assigned_top_level_lateinit_reads_back() {
    expect_box_same_as_kotlinc(
        &format!(
            r#"
{PAYLOAD}
lateinit var item: Payload
private lateinit var hidden: Payload

fun box(): String {{
    item = Payload("O")
    hidden = Payload("K")
    return item.token + hidden.token
}}
"#
        ),
        "TopLevelLateinitAssigned",
    );
}

#[test]
fn top_level_is_initialized_reads_the_field_without_throwing() {
    expect_box_same_as_kotlinc(
        &format!(
            r#"
{PAYLOAD}
lateinit var item: Payload
private lateinit var hidden: Payload

object C {{
    fun ready() = ::hidden.isInitialized
    fun set(value: Payload) {{ hidden = value }}
}}

fun box(): String {{
    if (::item.isInitialized) return "item already"
    if (C.ready()) return "hidden already"
    item = Payload("O")
    C.set(Payload("K"))
    return if (::item.isInitialized && C.ready()) item.token + "K" else "not ready"
}}
"#
        ),
        "TopLevelLateinitInitialized",
    );
}

#[test]
fn a_private_top_level_read_spills_an_earlier_operand() {
    expect_box_same_as_kotlinc(
        &format!(
            r#"
{PAYLOAD}
private lateinit var hidden: Payload

fun join(left: Payload, right: Payload): String = left.token + right.token

fun box(): String {{
    hidden = Payload("K")
    return join(Payload("O"), hidden)
}}
"#
        ),
        "TopLevelLateinitSpill",
    );
}

#[test]
fn top_level_lateinit_matches_kotlinc_bytecode() {
    let Some(facade) = compare_with_kotlinc_plugin(
        "TopLevelLateinit",
        SHAPES,
        "TopLevelLateinitKt",
        &[common::stdlib_jar()],
        "17",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in [
        "Payload getItem();",
        "java.lang.String box();",
        "Payload readHidden();",
        "Payload readInside();",
        "boolean ready();",
        "boolean hiddenReady();",
    ] {
        assert_eq!(
            method_instructions(&facade.krusty, member),
            method_instructions(&facade.reference, member),
            "{member}"
        );
    }
    let reference_fields = member_table(&facade.reference_bytes)
        .into_iter()
        .filter(|row| row.starts_with("field "))
        .collect::<Vec<_>>();
    let krusty_fields = member_table(&facade.krusty_bytes)
        .into_iter()
        .filter(|row| row.starts_with("field "))
        .collect::<Vec<_>>();
    assert_eq!(krusty_fields, reference_fields);

    let Some(holder) = compare_with_kotlinc_plugin(
        "TopLevelLateinit",
        SHAPES,
        "C",
        &[common::stdlib_jar()],
        "17",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in ["Payload getS();", "boolean ready();"] {
        assert_eq!(
            method_instructions(&holder.krusty, member),
            method_instructions(&holder.reference, member),
            "C.{member}"
        );
    }
}
