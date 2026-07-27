//! Enum entries are visible as bare names in their enum's lexical scope after higher-priority
//! locals, implicit receivers, and companion members have been considered.

use super::common;

#[test]
fn companion_members_resolve_bare_enum_entries() {
    const SOURCE: &str = r#"
enum class Choice {
    RETAIN,
    REPLACE;

    companion object {
        val defaultRule: (Boolean) -> Choice = { retain ->
            if (retain) RETAIN else REPLACE
        }

        fun choose(retain: Boolean): Choice =
            if (retain) RETAIN else REPLACE
    }
}
"#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "bare enum entries in companion members",
    );
}

#[test]
fn companion_property_shadows_enum_entry() {
    const SOURCE: &str = r#"
enum class Result {
    VALUE;

    fun entry(): Result = VALUE

    companion object {
        val VALUE: String = "shadow"
        fun text(): String = VALUE
    }
}
"#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "companion property shadows enum entry",
    );
}

#[test]
fn nested_companion_property_shadows_outer_enum_entry() {
    const SOURCE: &str = r#"
enum class Outer {
    VALUE;

    class Nested {
        companion object {
            val VALUE: String = "shadow"
        }

        fun text(): String = VALUE
    }
}
"#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "nested companion property shadows outer enum entry",
    );
}

#[test]
fn inner_receiver_property_shadows_enum_entry() {
    const SOURCE: &str = r#"
class Scope(val READY: String)

enum class State {
    READY;

    fun read(scope: Scope): String = with(scope) { READY }
}

fun box(): String = State.READY.read(Scope("OK"))
"#;

    common::expect_box_ok_with_stdlib(SOURCE, "EnumEntryReceiverShadow");
}

#[test]
fn member_extension_receiver_property_shadows_enum_entry() {
    const SOURCE: &str = r#"
class Scope(val READY: String)

enum class State {
    READY;

    fun Scope.read(): String = READY
}
"#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "member extension receiver shadows enum entry",
    );
}

#[test]
fn nested_lexical_class_resolves_enclosing_enum_entry() {
    const SOURCE: &str = r#"
enum class State {
    READY,
    WAITING;

    class Nested {
        fun initial(): State = READY
    }
}
"#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "bare enum entry in nested lexical class",
    );
}

#[test]
fn member_method_lowers_bare_enum_entry() {
    const SOURCE: &str = r#"
enum class State {
    READY,
    WAITING;

    fun initial(): State = READY
}

fun box(): String =
    if (State.WAITING.initial() == State.READY) "OK" else "wrong"
"#;

    common::expect_box_ok_with_stdlib(SOURCE, "BareEnumEntry");
}
