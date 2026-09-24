//! Iterator-protocol loops pass their checked subject directly to `iterator()`.
//!
//! Repository-owned protocol declarations keep this regression on the generic dispatch, extension,
//! and member-extension paths rather than a stdlib classifier that may acquire specialized lowering.

use super::common::{self, compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = r#"
var trace = 0

fun mark(digit: Int) {
    trace = trace * 10 + digit
}

class Cursor(private val end: Int) {
    private var current = 0

    operator fun hasNext(): Boolean = current < end

    operator fun next(): Int {
        val value = current
        current++
        return value
    }
}

class DispatchRange(private val end: Int) {
    operator fun iterator(): Cursor {
        mark(2)
        return Cursor(end)
    }
}

fun dispatchSubject(): DispatchRange {
    mark(1)
    return DispatchRange(3)
}

fun dispatchLoop(): Int {
    var sum = 0
    for (value in dispatchSubject()) sum += value
    return sum
}

class ExtensionRange(val end: Int)

fun extensionSubject(): ExtensionRange {
    mark(1)
    return ExtensionRange(3)
}

operator fun ExtensionRange.iterator(): Cursor {
    mark(2)
    return Cursor(end)
}

fun extensionLoop(): Int {
    var sum = 0
    for (value in extensionSubject()) sum += value
    return sum
}

class MemberRange(val end: Int)

fun memberSubject(): MemberRange {
    mark(1)
    return MemberRange(3)
}

class RangeScope {
    operator fun MemberRange.iterator(): Cursor {
        mark(2)
        return Cursor(end)
    }

    fun memberExtensionLoop(): Int {
        var sum = 0
        for (value in memberSubject()) sum += value
        return sum
    }
}

fun box(): String {
    trace = 0
    if (dispatchLoop() != 3 || trace != 12) return "FAIL dispatch: $trace"
    trace = 0
    if (extensionLoop() != 3 || trace != 12) return "FAIL extension: $trace"
    trace = 0
    if (RangeScope().memberExtensionLoop() != 3 || trace != 12) {
        return "FAIL member extension: $trace"
    }
    return "OK"
}
"#;

fn assert_methods_match(built: &common::ReferenceComparison, methods: &[&str]) {
    for method in methods {
        let reference = method_instructions(&built.reference, method);
        assert!(
            !reference.is_empty(),
            "{method} not found in kotlinc output"
        );
        assert_eq!(
            method_instructions(&built.krusty, method),
            reference,
            "{method}"
        );
    }
}

#[test]
fn iterator_protocol_receivers_take_kotlincs_slots() {
    let classpath = [common::stdlib_jar()];
    let Some(top_level) = compare_with_kotlinc_plugin(
        "IteratorProtocolSlots",
        SOURCE,
        "IteratorProtocolSlotsKt",
        &classpath,
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_methods_match(&top_level, &["int dispatchLoop(", "int extensionLoop("]);

    let Some(member_extension) = compare_with_kotlinc_plugin(
        "IteratorProtocolSlots",
        SOURCE,
        "RangeScope",
        &classpath,
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    assert_methods_match(&member_extension, &["int memberExtensionLoop("]);
}

#[test]
fn iterator_protocol_subject_is_evaluated_once_before_iterator() {
    common::expect_box_same_as_kotlinc(SOURCE, "IteratorProtocolOrder");
}
