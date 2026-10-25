//! A private property read or written from a non-private `inline` function of its file.
//!
//! The inline body runs in whichever class inlines it, so kotlinc reaches the property through
//! its owner's public synthetic accessors: `access$get<X>$p` / `access$set<X>$p` for the backing
//! field, `access$get<X>` / `access$set<X>` for an accessor the source declares or a delegate
//! supplies, an instance accessor taking the owner first, and a `lateinit` read guarded at the use
//! site. Calling the property's own `getX`, which a private property does not publish, fails with
//! `NoSuchMethodError` when the copy runs in another file's class.

use super::common;

const DECLARATION: &str = r#"package p

private val hidden = "h"
private var count = 0
private val computed: String get() = "c$count"
private var custom: Int = 1
    get() = field + 1
    set(value) { field = value * 2 }
private val lazyText by lazy { "z" }
private lateinit var late: String
private const val K = "k"

internal inline fun reveal(): String {
    count += 1
    custom = 5
    late = "l"
    return hidden + count + computed + custom + lazyText + late + K
}
"#;

const CALLER: &str = r#"package p

fun box(): String {
    val revealed = reveal()
    if (revealed != "h1c111zlk") return revealed
    return "OK"
}
"#;

const SOURCES: [(&str, &str); 2] = [("Declaration.kt", DECLARATION), ("Caller.kt", CALLER)];

#[test]
fn a_private_property_in_an_inline_body_is_reached_through_its_accessors_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&SOURCES);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn a_private_property_in_an_inline_body_inlined_into_another_file_runs() {
    common::expect_box_ok_files_with_stdlib(&SOURCES, "Caller");
}

/// A member's private property is reached through its class's static accessor, which takes the
/// instance first.
const MEMBER_DECLARATION: &str = r#"package p

class Holder {
    private val inside = "i"
    private var mutable = 0

    internal inline fun read(): String {
        mutable += 1
        return inside + mutable
    }
}
"#;

const MEMBER_CALLER: &str = r#"package p

fun box(): String {
    val read = Holder().read()
    return if (read == "i1") "OK" else read
}
"#;

const MEMBER_SOURCES: [(&str, &str); 2] = [
    ("Holder.kt", MEMBER_DECLARATION),
    ("MemberCaller.kt", MEMBER_CALLER),
];

#[test]
fn a_private_member_property_in_an_inline_body_is_reached_through_its_instance_accessors() {
    let holder = common::ModuleClassPair::compile(&MEMBER_SOURCES, "p/Holder");
    assert!(
        holder.kotlinc == holder.krusty,
        "p/Holder differs from kotlinc's bytes"
    );
    let caller = common::ModuleClassPair::compile(&MEMBER_SOURCES, "p/MemberCallerKt");
    let (reference, krusty) = caller.method_code("p/MemberCallerKt", "box");
    assert_eq!(krusty, reference, "p/MemberCallerKt.box");
}

#[test]
fn a_private_member_property_in_an_inline_body_inlined_into_another_file_runs() {
    common::expect_box_ok_files_with_stdlib(&MEMBER_SOURCES, "MemberCaller");
}
