//! A top-level `lateinit var` throws `UninitializedPropertyAccessException` until it is assigned.
//!
//! A public or internal one is a public static field whose getter throws. A private one keeps a
//! private field; the same file inlines the guard, and another class does so after `access$get…$p`.
//! `::prop.isInitialized` reads the field without that guard.

use super::common::{
    self, compare_with_kotlinc_plugin, expect_box_same_as_kotlinc, member_table,
    method_instructions,
};

const SHAPES: &str = r#"
lateinit var str: String
private lateinit var hidden: String
internal lateinit var inside: String

fun readHidden(): String = hidden
fun ready(): Boolean = ::str.isInitialized
fun hiddenReady(): Boolean = ::hidden.isInitialized
fun box(): String = str
fun readInside(): String = inside

object C {
    fun getS(): String = hidden
    fun ready(): Boolean = ::hidden.isInitialized
}
"#;

#[test]
fn an_uninitialized_top_level_read_throws() {
    expect_box_same_as_kotlinc(
        r#"
@file:Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
import kotlin.UninitializedPropertyAccessException

lateinit var str: String

fun box(): String {
    try {
        val str2 = str
        return "Should throw an exception, got $str2"
    } catch (e: UninitializedPropertyAccessException) {
        return "OK"
    } catch (e: Throwable) {
        return "Unexpected exception: ${e::class}"
    }
}
"#,
        "TopLevelLateinitRead",
    );
}

#[test]
fn an_uninitialized_top_level_member_access_throws() {
    expect_box_same_as_kotlinc(
        r#"
@file:Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
import kotlin.UninitializedPropertyAccessException

lateinit var str: String

fun box(): String {
    try {
        val i = str.length
        return "Should throw an exception, got $i"
    } catch (e: UninitializedPropertyAccessException) {
        return "OK"
    } catch (e: Throwable) {
        return "Unexpected exception: ${e::class}"
    }
}
"#,
        "TopLevelLateinitMember",
    );
}

#[test]
fn a_private_top_level_read_from_another_class_throws() {
    expect_box_same_as_kotlinc(
        r#"
@file:Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
import kotlin.UninitializedPropertyAccessException

private lateinit var s: String

object C {
    fun getS() = s
}

fun box(): String {
    try {
        val str2 = C.getS()
        return "Should throw an exception, got $str2"
    } catch (e: UninitializedPropertyAccessException) {
        return "OK"
    } catch (e: Throwable) {
        return "Unexpected exception: ${e::class}"
    }
}
"#,
        "TopLevelLateinitBridge",
    );
}

#[test]
fn an_assigned_top_level_lateinit_reads_back() {
    expect_box_same_as_kotlinc(
        r#"
lateinit var str: String
private lateinit var hidden: String

fun box(): String {
    str = "O"
    hidden = "K"
    return str + hidden
}
"#,
        "TopLevelLateinitAssigned",
    );
}

#[test]
fn top_level_is_initialized_reads_the_field_without_throwing() {
    expect_box_same_as_kotlinc(
        r#"
lateinit var str: String
private lateinit var hidden: String

object C {
    fun ready() = ::hidden.isInitialized
    fun set(value: String) { hidden = value }
}

fun box(): String {
    if (::str.isInitialized) return "str already"
    if (C.ready()) return "hidden already"
    str = "O"
    C.set("K")
    return if (::str.isInitialized && C.ready()) str + "K" else "not ready"
}
"#,
        "TopLevelLateinitInitialized",
    );
}

#[test]
fn a_private_top_level_read_spills_an_earlier_operand() {
    expect_box_same_as_kotlinc(
        r#"
private lateinit var hidden: String

fun box(): String {
    hidden = "K"
    val parts = listOf("O", hidden)
    return parts[0] + parts[1]
}
"#,
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
        "java.lang.String getStr();",
        "java.lang.String box();",
        "java.lang.String readHidden();",
        "java.lang.String readInside();",
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
    for member in ["java.lang.String getS();", "boolean ready();"] {
        assert_eq!(
            method_instructions(&holder.krusty, member),
            method_instructions(&holder.reference, member),
            "C.{member}"
        );
    }
}
