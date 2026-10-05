//! Smart casts and metadata from Kotlin contracts.
//!
//! A `returns(…) implies <condition>` effect narrows the caller's arguments where the call's value
//! is known: a Boolean call in a condition proves `returns(true)` or `returns(false)`, and a call
//! compared with `null` proves `returnsNotNull()` or `returns(null)`. A type conclusion narrows the
//! same way an `is` test does, a primitive type included. A final member function's contract
//! narrows like a top-level one. kotlinc records each contract in `@Metadata`, its `is` types in
//! the containing declaration's type table.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and each
//! class file is compared byte for byte.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

const NARROWING: &str = r#"
@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
import kotlin.contracts.*

fun isText(value: Any?): Boolean {
    contract { returns(true) implies (value is String) }
    return value is String
}
fun notNumber(value: Any?): Boolean {
    contract { returns(false) implies (value is Int) }
    return value !is Int
}
fun same(value: String?): String? {
    contract { returnsNotNull() implies (value != null) }
    return value
}
fun pair(first: Any?, second: Any?): Boolean {
    contract { returns(true) implies (first is String && second is Int) }
    return first is String && second is Int
}
fun mixed(first: Any?, second: Any?, third: Any?): Boolean {
    contract { returns(true) implies ((first is String || second is Int) && third != null) }
    return third != null
}

fun textLength(value: Any?): Int = if (isText(value)) value.length else 0
fun increment(value: Any?): Int = if (!notNumber(value)) value + 1 else 0
fun lengthOrZero(value: String?): Int = if (same(value) != null) value.length else 0
fun lengthOrMinus(value: String?): Int = if (same(value) == null) -1 else value.length
fun sum(first: Any?, second: Any?): Int = if (pair(first, second)) first.length + second else 0
"#;

#[test]
fn a_contract_narrows_reference_and_primitive_types() {
    byte_identical("Narrowing", NARROWING, "NarrowingKt");
}

const MEMBER: &str = r#"
@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
import kotlin.contracts.*

class Checks {
    fun isText(value: Any?): Boolean {
        contract { returns(true) implies (value is String) }
        return value is String
    }
    fun isCount(value: Any?): Boolean {
        contract { returns(true) implies (value is Int) }
        return value is Int
    }
}

fun measure(checks: Checks, value: Any?): Int =
    if (checks.isText(value)) value.length else if (checks.isCount(value)) value else 0
"#;

#[test]
fn a_member_contract_narrows_and_is_recorded_in_class_metadata() {
    for class in ["Checks", "MemberKt"] {
        byte_identical("Member", MEMBER, class);
    }
}

/// The contract call and its invocation kinds are bound by the declarations they select, so a
/// package-qualified `contract`, an import-aliased `InvocationKind`, a fully qualified entry, and
/// an explicitly imported entry all declare the same contract as the plain spelling.
const QUALIFIED_SPELLINGS: &str = r#"
@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
import kotlin.contracts.*
import kotlin.contracts.InvocationKind.EXACTLY_ONCE
import kotlin.contracts.InvocationKind as Kind

fun qualified(x: Any?): Boolean {
    kotlin.contracts.contract { returns(true) implies (x is String) }
    return x is String
}
fun importedEntry(block: () -> Unit) {
    contract { callsInPlace(block, EXACTLY_ONCE) }
    block()
}
fun aliasedKind(block: () -> Unit) {
    contract { callsInPlace(block, Kind.AT_MOST_ONCE) }
    block()
}
fun fullyQualified(block: () -> Unit) {
    contract { callsInPlace(block, kotlin.contracts.InvocationKind.AT_LEAST_ONCE) }
    block()
}
fun use(a: Any?): Int = if (qualified(a)) a.length else 0
"#;

#[test]
fn qualified_and_aliased_spellings_declare_the_same_contract() {
    byte_identical("Qualified", QUALIFIED_SPELLINGS, "QualifiedKt");
}

/// An entry reached through a star import of `InvocationKind` and one imported under an alias are
/// the same entries; the binding keeps the entry the ordinary value selection chose.
const IMPORTED_KINDS: &str = r#"
@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
import kotlin.contracts.*
import kotlin.contracts.InvocationKind.*
import kotlin.contracts.InvocationKind.AT_MOST_ONCE as Once

fun starImported(block: () -> Unit) {
    contract { callsInPlace(block, EXACTLY_ONCE) }
    block()
}
fun aliasedEntry(block: () -> Unit) {
    contract { callsInPlace(block, Once) }
    block()
}
"#;

#[test]
fn star_imported_and_aliased_invocation_kinds_declare_their_entries() {
    byte_identical("ImportedKinds", IMPORTED_KINDS, "ImportedKindsKt");
}

/// A contract names its owner's parameters, so each conclusion narrows the argument the call's
/// committed mapping gives that parameter, never the argument in the same source position: a value
/// after a defaulted parameter, and named arguments in another order.
const MAPPED_ARGUMENTS: &str = r#"
@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
import kotlin.contracts.*

fun afterDefault(prefix: String = "", value: Any?): Boolean {
    contract { returns(true) implies (value is Int) }
    return value is Int
}

fun defaulted(a: Any?): Int = if (afterDefault(value = a)) a + 1 else 0
"#;

const REORDERED_ARGUMENTS: &str = r#"
@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
import kotlin.contracts.*

fun second(first: Any?, second: Any?): Boolean {
    contract { returns(true) implies (second is String) }
    return second is String
}

fun reordered(a: Any?, b: Any?): Int = if (second(second = a, first = b)) a.length else 0
fun box(): String = if (reordered("OK", 1) == 2) "OK" else "FAIL"
"#;

const EXPLICIT_EXTENSION_CONTEXT: &str = r#"
// LANGUAGE: +ContextParameters +ExplicitContextArguments
@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
import kotlin.contracts.*

context(first: Any?, second: Any?)
fun String.firstIsString(): Boolean {
    contract { returns(true) implies (first is String) }
    return first is String
}

fun explicitFirst(a: Any?, b: Any?): Int = with(b) {
    if ("receiver".firstIsString(first = a)) a.length else 0
}

fun box(): String = if (explicitFirst("OK", 1) == 2) "OK" else "FAIL"
"#;

#[test]
fn a_conclusion_narrows_the_argument_mapped_to_its_parameter() {
    byte_identical("MappedArguments", MAPPED_ARGUMENTS, "MappedArgumentsKt");
    common::assert_accepted_like_kotlinc(REORDERED_ARGUMENTS);
    assert_eq!(
        common::expect_box_run_with_stdlib(EXPLICIT_EXTENSION_CONTEXT, "ExplicitExtensionContext"),
        "OK"
    );
}
