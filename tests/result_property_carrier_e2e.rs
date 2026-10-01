//! A property declared as `Result<T>` stores Result's carrier, not a `kotlin.Result` box.
//!
//! `data class Foo(val result: Result<Boolean>)` keeps the success value (`Boolean`) in an
//! `Object` field. Reading it and calling `getOrNull()` must pass that carrier to
//! `Result.isFailure-impl(Object)`. Classifying the erased `Object` stamp as a generic box
//! inserts `checkcast kotlin/Result; unbox-impl` and throws
//! `ClassCastException: Boolean cannot be cast to kotlin.Result`.
//!
//! `class Box<T>(val value: T)` read as `Box<Result<Boolean>>` is the generic slot: the
//! declaration is `T`, so the value stays boxed. A member extension that shares the property
//! name keeps `getResult`; the value-class mangling belongs to the `Result` property's accessor.
//! Corpus `dataClasses/components/kt49812.kt`.
use super::common::{self, compare_with_kotlinc_plugin, member_table, method_instructions};

const FOO: &str = "\
data class Foo(val result: Result<Boolean>) {\n\
    val Boolean.result: String\n\
        get() = if (this) \"OK\" else \"Fail\"\n\
\n\
    fun f() = result.getOrNull()!!.result\n\
}\n\
\n\
fun box() = Foo(Result.success(true)).f()\n";

#[test]
fn a_result_property_read_uses_the_carrier() {
    common::expect_box_ok_with_stdlib(FOO, "ResultPropertyCarrier");
}

#[test]
fn a_result_property_read_matches_kotlinc_until_is_failure() {
    let Some(built) = compare_with_kotlinc_plugin(
        "ResultPropertyCarrier",
        FOO,
        "Foo",
        &[common::stdlib_jar()],
        "21",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let krusty = method_instructions(&built.krusty, "String f();");
    let reference = method_instructions(&built.reference, "String f();");
    assert!(
        krusty.iter().all(|line| !line.contains("unbox-impl")),
        "f() unboxed a carrier that is already unboxed: {krusty:?}"
    );
    assert!(
        krusty
            .iter()
            .all(|line| !(line.contains("checkcast") && line.contains("kotlin/Result"))),
        "f() cast the carrier to kotlin.Result: {krusty:?}"
    );
    assert_eq!(
        until_is_failure(&krusty),
        until_is_failure(&reference),
        "f() carrier read"
    );
    assert_eq!(
        getter_invokes(&krusty),
        getter_invokes(&reference),
        "f() accessor calls"
    );
    assert!(
        getter_invokes(&krusty)
            .iter()
            .any(|line| line.contains("getResult:") && !line.contains("getResult-")),
        "the member extension keeps getResult: {:?}",
        getter_invokes(&krusty)
    );
    let krusty_accessors = accessor_members(&built.krusty_bytes);
    let reference_accessors = accessor_members(&built.reference_bytes);
    assert_eq!(
        krusty_accessors, reference_accessors,
        "Result accessor and the same-named extension"
    );
    assert!(
        krusty_accessors
            .iter()
            .any(|row| row.contains("getResult(") && !row.contains("getResult-")),
        "the member extension stays getResult: {krusty_accessors:?}"
    );
}

fn accessor_members(bytes: &[u8]) -> Vec<String> {
    member_table(bytes)
        .into_iter()
        .filter(|row| row.contains("getResult") || row.contains(" result "))
        .collect()
}

const GENERIC_SLOT: &str = "\
class Box<T>(val value: T)\n\
fun box(): String {\n\
    val result = Box(Result.success(true)).value\n\
    return if (result.getOrNull() == true) \"OK\" else \"FAIL\"\n\
}\n";

#[test]
fn a_generic_result_slot_stays_boxed() {
    common::expect_box_ok_with_stdlib(GENERIC_SLOT, "GenericResultSlot");
    let Some(read) = compare_with_kotlinc_plugin(
        "GenericResultSlot",
        GENERIC_SLOT,
        "GenericResultSlotKt",
        &[common::stdlib_jar()],
        "21",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let krusty = method_instructions(&read.krusty, "String box();");
    let reference = method_instructions(&read.reference, "String box();");
    assert_eq!(
        until_is_failure(&krusty),
        until_is_failure(&reference),
        "generic Result slot read"
    );
    assert!(
        krusty
            .iter()
            .any(|line| line.contains("checkcast") && line.contains("kotlin/Result")),
        "the generic slot is read as a kotlin.Result box: {krusty:?}"
    );
    assert!(
        krusty.windows(2).any(|pair| {
            pair[0].contains("checkcast")
                && pair[0].contains("kotlin/Result")
                && pair[1].contains("unbox-impl")
        }),
        "the box is unboxed only after it is checked: {krusty:?}"
    );
    let Some(owner) = compare_with_kotlinc_plugin(
        "GenericResultSlot",
        GENERIC_SLOT,
        "Box",
        &[common::stdlib_jar()],
        "21",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let krusty_members = value_members(&owner.krusty_bytes);
    let reference_members = value_members(&owner.reference_bytes);
    assert_eq!(krusty_members, reference_members, "generic value accessor");
    assert!(
        krusty_members
            .iter()
            .any(|row| row.contains("getValue()") && !row.contains("getValue-")),
        "the generic getter stays getValue: {krusty_members:?}"
    );
}

fn value_members(bytes: &[u8]) -> Vec<String> {
    member_table(bytes)
        .into_iter()
        .filter(|row| row.contains("value") || row.contains("Value"))
        .collect()
}

fn until_is_failure(instructions: &[String]) -> Vec<&String> {
    let end = instructions
        .iter()
        .position(|line| line.contains("isFailure-impl"))
        .expect("isFailure-impl");
    instructions[..=end].iter().collect()
}

fn getter_invokes(instructions: &[String]) -> Vec<&String> {
    instructions
        .iter()
        .filter(|line| line.contains("getResult"))
        .collect()
}
