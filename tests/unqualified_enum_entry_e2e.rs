use super::common;

fn assert_accepted(sources: &[(&str, &str)]) {
    let result = common::compiler_diagnostics(sources, &[common::stdlib_jar()]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, "")
    );
    let source_text = sources
        .iter()
        .map(|(_, source)| *source)
        .collect::<Vec<_>>();
    assert_eq!(
        common::front_end_diagnostics_files_with_stdlib(&source_text),
        Vec::<String>::new()
    );
}

fn assert_accepted_and_runs(source: &str, stem: &str) {
    assert_accepted(&[("Main.kt", source)]);
    common::expect_box_ok_with_stdlib(source, stem);
}

fn assert_accepted_and_runs_files(sources: &[(&str, &str)], stem: &str) {
    assert_accepted(sources);
    common::expect_box_ok_files_with_stdlib(sources, stem);
}

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
fn enum_entry_constructor_lambda_resolves_its_entry() {
    const SOURCE: &str = r#"
enum class Choice(val text: String, val callback: () -> String) {
    RETAIN("OK", { RETAIN.text })
}
"#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "bare enum entry in its constructor lambda",
    );
}

#[test]
fn enum_entry_constructor_object_resolves_its_entry() {
    const SOURCE: &str = r#"
interface Callback { fun invoke(): String }

enum class Choice(val text: String, val callback: Callback) {
    RETAIN("OK", object : Callback {
        override fun invoke(): String = RETAIN.text
    })
}
"#;

    common::expect_front_end_ok_files_with_stdlib(
        &[SOURCE],
        "bare enum entry in its constructor object",
    );
}

#[test]
fn constructor_header_this_is_the_selected_companion() {
    const SOURCE: &str = r#"
open class Base(val callback: () -> Any) {
    companion object { val marker = "base" }
}

class Derived : Base({ this })

class Secondary : Base {
    constructor() : super({ this })
}

class ThisDelegating(val callback: () -> Any) {
    constructor() : this({ this })
    companion object { val marker = "this" }
}

enum class Choice(val callback: () -> Any) {
    RETAIN({ this })
}

fun box(): String =
    if (Derived().callback() === Base &&
        Secondary().callback() === Base &&
        ThisDelegating().callback() === ThisDelegating &&
        Choice.RETAIN.callback() === Enum) "OK" else "wrong"
"#;

    common::expect_box_ok_with_stdlib(SOURCE, "ConstructorHeaderThis");
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

#[test]
fn imported_private_enum_entry_resolves_unqualified() {
    const SOURCE: &str = r#"
package p

import p.StackframeShrinkVerdict.KEEP
import p.StackframeShrinkVerdict.OMIT

private enum class StackframeShrinkVerdict {
    KEEP,
    OMIT,
}

private fun judge(value: Int): StackframeShrinkVerdict =
    if (value == 0) OMIT else KEEP

fun box(): String =
    if (judge(1) == KEEP && judge(0) == OMIT) "OK" else "wrong"
"#;

    assert_accepted_and_runs(SOURCE, "ImportedPrivateEnumEntry");
}

#[test]
fn imported_enum_entry_resolves_across_files() {
    const ENUM_FILE: &str = r#"
package p

enum class Choice {
    RETAIN,
    REPLACE,
}
"#;
    const MAIN_FILE: &str = r#"
package p

import p.Choice.RETAIN
import p.Choice.REPLACE

fun box(): String {
    val picked = if (System.currentTimeMillis() >= 0L) RETAIN else REPLACE
    return if (picked == RETAIN && picked != REPLACE) "OK" else "wrong"
}
"#;

    assert_accepted_and_runs_files(
        &[("Choice.kt", ENUM_FILE), ("Main.kt", MAIN_FILE)],
        "enum entry imported across files",
    );
}

#[test]
fn aliased_enum_entry_import_resolves() {
    const SOURCE: &str = r#"
package p

import p.Choice.REPLACE as Substitute

enum class Choice {
    RETAIN,
    REPLACE,
}

fun box(): String =
    if (Substitute == Choice.REPLACE) "OK" else "wrong"
"#;

    assert_accepted_and_runs(SOURCE, "AliasedEnumEntryImport");
}

#[test]
fn local_value_shadows_imported_enum_entry() {
    const SOURCE: &str = r#"
package p

import p.Choice.RETAIN

enum class Choice { RETAIN }

fun box(): String {
    val RETAIN = "OK"
    return RETAIN
}
"#;

    assert_accepted_and_runs(SOURCE, "ImportedEnumEntryShadow");
}

#[test]
fn aliased_enum_entry_is_exhaustive_in_when() {
    const SOURCE: &str = r#"
package p

import p.Choice.RETAIN as Keep

enum class Choice { RETAIN, REPLACE }

fun describe(choice: Choice): String = when (choice) {
    Keep -> "OK"
    Choice.REPLACE -> "wrong"
}

fun box(): String = describe(Keep)
"#;

    assert_accepted_and_runs(SOURCE, "AliasedEnumEntryWhen");
}

#[test]
fn aliased_classpath_enum_entry_folds_in_annotation() {
    const SOURCE: &str = r#"
import kotlin.annotation.AnnotationRetention.RUNTIME as RuntimeRetention

@Retention(RuntimeRetention)
annotation class Marker

fun box(): String =
    if (RuntimeRetention == AnnotationRetention.RUNTIME) "OK" else "wrong"
"#;

    assert_accepted_and_runs(SOURCE, "ClasspathEnumEntryImport");
}
