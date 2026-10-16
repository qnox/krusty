//! A library scope function lowered from its declaration plan (`run`, `let`, and owned shapes
//! that return the lambda's result or their receiver) keeps the call site kotlinc's inliner
//! writes: the opening `nop` on the call line when no operand already carries that line, the
//! `$i$f$`/`$i$a$` markers with their local-variable rows, and the fake.kt SMAP stratum that maps
//! the inlined lines, byte-for-byte.

use super::common;

#[test]
fn a_run_call_is_inlined_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Run.kt",
        r#"
fun g(): Int = run { 1 }
"#,
    )]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn a_let_call_with_a_captured_value_is_inlined_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Let.kt",
        r#"
fun shifted(value: Int, offset: Int): Int = value.let { it + offset }
"#,
    )]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const LIB: &str = r#"
inline fun <T, R> T.convert(block: (T) -> R): R = block(this)

inline fun <T> T.observe(block: (T) -> Unit): T {
    block(this)
    return this
}
"#;

const MAIN: &str = r#"
var seen = 0

fun converted(value: Int, offset: Int): Int = value.convert { it + offset }

fun observed(value: String): String = value.observe { seen = it.length }

fun box(): String {
    val converted = converted(40, 2)
    val observed = observed("OK")
    return if (converted == 42 && seen == 2) observed else "FAIL $converted $seen"
}
"#;

#[test]
fn owned_scope_shapes_run() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}

#[test]
fn owned_scope_shapes_are_inlined_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_lib("Main", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// The method after a top-level setter is rewritten (the inlined `let` drops its opening `nop`).
/// The setter's `<set-?>` local name stays with the setter in the constant pool, ahead of that
/// method's entries, where kotlinc's writer interns it.
#[test]
fn a_setter_local_name_keeps_its_place_before_a_rewritten_inline_call() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Setter.kt",
        r#"
var seen = 0

fun converted(value: Int, offset: Int): Int = value.let { it + offset }
"#,
    )]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// A statement-position `with`/`run` whose lambda is a multi-line `Unit` body. After the lambda's
/// closing-brace `nop`, while its locals are open, kotlinc's inliner returns to the call's line on
/// a `nop` of its own once the lambda locals end. The redundant-`nop` sweep keeps it: nothing else
/// is on that line before the next statement's.
#[test]
fn a_statement_unit_lambda_returns_to_the_call_line_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Statement.kt",
        r#"
var log = ""

fun record(text: String) {
    log += text
}

fun box(): String {
    with("O") {
        record(this)
        record(length.toString())
    }
    run {
        record("K")
        record("!")
    }
    return log
}
"#,
    )]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

fn assert_inline_result_like_kotlinc(file: &str, source: &str) {
    let classes = common::classes_against_kotlinc_module(&[(file, source)]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn an_erased_same_file_inline_result_is_narrowed_after_its_frame() {
    assert_inline_result_like_kotlinc(
        "Narrowed.kt",
        r#"
inline fun <T, R> T.pass(f: (T) -> R): R = f(this)

fun use(a: String?, f: (String) -> String): String? {
    val r = a?.pass(f)
    return r
}
"#,
    );
}

#[test]
fn a_unit_same_file_inline_result_is_materialized_after_its_frame() {
    assert_inline_result_like_kotlinc(
        "UnitResult.kt",
        r#"
inline fun <T, R> T.pass(f: (T) -> R): R = f(this)

fun f(s: String) { }

fun use(a: String?): Unit? {
    val r = a?.pass { f(it) }
    return r
}
"#,
    );
}

const RECORDED_INLINE_RESULT: &str = r#"
inline fun <T, R> T.pass(f: (T) -> R): R = f(this)

var seen = ""

fun record(text: String) {
    seen += text
}

fun box(): String {
    val a: String? = "OK"
    val r = a?.pass { record(it) }
    return if (r == Unit) seen else "FAIL"
}
"#;

#[test]
fn a_recorded_unit_same_file_inline_result_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(RECORDED_INLINE_RESULT, "RecordedInlineResult"),
        "OK"
    );
}

#[test]
fn a_recorded_unit_same_file_inline_result_matches_kotlinc_bytes() {
    assert_inline_result_like_kotlinc("RecordedInlineResult.kt", RECORDED_INLINE_RESULT);
}

#[test]
fn a_discarded_same_file_inline_result_is_popped_after_its_frame() {
    assert_inline_result_like_kotlinc(
        "Discarded.kt",
        r#"
inline fun <T, R> T.pass(f: (T) -> R): R = f(this)

fun stmt(a: String, f: (String) -> String) {
    a.pass(f)
    a.pass(f)
}

inline fun twice(f: () -> Int): Int = f() + f()

fun t(f: () -> Int) {
    twice(f)
    twice(f)
}
"#,
    );
}

#[test]
fn discarded_non_call_inline_results_are_popped_after_their_frames() {
    assert_inline_result_like_kotlinc(
        "DiscardedValues.kt",
        r#"
class Holder(val value: String)

inline fun <T> local(value: T): T = value
inline fun field(holder: Holder): String = holder.value
inline fun created(): Holder = Holder("new")

object StaticValue {
    private var top = "top"
    private inline fun value(): String = top

    fun discard() {
        value()
    }
}

fun discard(value: String, holder: Holder) {
    local(value)
    field(holder)
    created()
}
"#,
    );
}

#[test]
fn a_discarded_provider_result_is_popped_after_the_enclosing_copied_frame() {
    assert_inline_result_like_kotlinc(
        "DiscardedProvider.kt",
        r#"
inline fun <T, R> T.pass(f: (T) -> R): R = this.let(f)

fun discard(value: String, f: (String) -> String) {
    value.pass(f)
    value.pass(f)
}
"#,
    );
}

#[test]
fn a_discarded_result_inside_a_nonempty_tail_block_survives_the_copied_frame() {
    assert_inline_result_like_kotlinc(
        "DiscardedTailBlock.kt",
        r#"
inline fun through(value: String): String {
    value.length
    return value
}

fun discard(value: String) {
    through(value)
    through(value)
}
"#,
    );
}

/// The provider-planned `let` invocation already owns the point where its erased result is
/// narrowed. Copying the containing same-file inline function must not claim that invocation a
/// second time when it becomes the copied frame's trailing value.
#[test]
fn a_provider_planned_result_nested_in_a_same_file_frame_has_one_adaptation_owner() {
    assert_inline_result_like_kotlinc(
        "NestedOwnership.kt",
        r#"
inline fun <T, R> T.pass(f: (T) -> R): R = this.let(f)

fun use(value: String, f: (String) -> String): String {
    val result = value.pass(f)
    return result
}
"#,
    );
}
