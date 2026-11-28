//! What a null check on a stable value proves through the value's initializer.
//!
//! `val selector = selectorFor(source)` carries the call's `returnsNotNull()` effect: every proof
//! that `selector` is not null (`!= null`, `!!`, a `?.` receiver, `?: return`, `requireNotNull`,
//! `checkNotNull`) proves the effect's conclusion, exactly as the same proof on the call would. A
//! `Boolean` val property assigned a condition in `init` carries the condition's facts the same
//! way. Each case is compared with kotlinc, including the forms that prove nothing: a `var`
//! holder, a root reassigned after the val, and a member val read from a member function, and a
//! root a closure mutates, which declines the cast.

use super::common;

const PRELUDE: &str = r#"import kotlin.contracts.*

interface Source {
    fun id(): String
}
class Selectable(val name: String) : Source {
    override fun id() = name
    fun select() = name
}
class Plain : Source {
    override fun id() = "p"
}

@OptIn(ExperimentalContracts::class)
fun selectorFor(source: Source): String? {
    contract { returnsNotNull() implies (source is Selectable) }
    return if (source is Selectable) source.name else null
}
"#;

fn with_prelude(body: &str) -> String {
    format!("{PRELUDE}\n{body}")
}

fn assert_rejected_like_kotlinc(body: &str) {
    common::assert_errors_match_kotlinc(&[("Main.kt", &with_prelude(body))], &[]);
}

#[test]
fn every_non_null_proof_of_the_val_proves_the_contract() {
    common::assert_accepted_like_kotlinc(&with_prelude(
        r#"
fun required(source: Source): String {
    val selector = selectorFor(source)
    requireNotNull(selector)
    return source.select()
}
fun checked(source: Source): String {
    val selector = selectorFor(source)
    checkNotNull(selector)
    return source.select()
}
fun asserted(source: Source): String {
    val selector = selectorFor(source)
    selector!!
    return source.select()
}
fun received(source: Source): String? {
    val selector = selectorFor(source)
    return selector?.let { source.select() }
}
fun elvisReturn(source: Source): String {
    val selector = selectorFor(source)
    selector ?: return "plain"
    return source.select()
}
fun elvisInitializer(source: Source): String {
    val selector = selectorFor(source) ?: return "plain"
    return source.select() + selector
}
"#,
    ));
}

#[test]
fn a_boolean_val_property_assigned_in_init_carries_its_condition() {
    common::assert_accepted_like_kotlinc(
        r#"
class Measured(val text: String?) {
    val present: Boolean
    val length: Int

    init {
        present = text != null
        length = if (present) text.length else 0
    }
}
"#,
    );
}

#[test]
fn a_var_holder_proves_nothing() {
    assert_rejected_like_kotlinc(
        r#"
fun held(source: Source): String {
    var selector = selectorFor(source)
    if (selector != null) return source.select()
    return "plain"
}
"#,
    );
}

#[test]
fn a_root_reassigned_after_the_val_proves_nothing() {
    assert_rejected_like_kotlinc(
        r#"
fun reassigned(initial: Source): String {
    var source = initial
    val selector = selectorFor(source)
    source = Plain()
    if (selector != null) return source.select()
    return "plain"
}
"#,
    );
}

/// The facts are recorded while the root is still stable; a closure that mutates the root before
/// the val is read declines them there with SMARTCAST_IMPOSSIBLE, for a contract effect and for
/// a safe-call chain alike.
#[test]
fn a_root_a_closure_mutates_declines_the_cast() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            r#"import kotlin.contracts.*

@OptIn(ExperimentalContracts::class)
fun present(text: String?): String? {
    contract { returnsNotNull() implies (text != null) }
    return text
}

fun contracted(initial: String?): Int {
    var text = initial
    val shown = present(text)
    val reset = { text = null }
    reset()
    if (shown != null) return text.length
    return 0
}

class Item(val name: String)

fun safeCalled(initial: Item?): String {
    var item = initial
    val name = item?.name
    val reset = { item = null }
    reset()
    if (name != null) return item.name
    return ""
}
"#,
        )],
        &[],
    );
}

#[test]
fn a_root_a_closure_mutates_before_the_val_proves_nothing() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            r#"import kotlin.contracts.*

@OptIn(ExperimentalContracts::class)
fun present(text: String?): String? {
    contract { returnsNotNull() implies (text != null) }
    return text
}

fun contracted(initial: String?): Int {
    var text = initial
    val reset = { text = null }
    val shown = present(text)
    if (shown != null) return text.length
    reset()
    return 0
}

class Item(val name: String)

fun safeCalled(initial: Item?): String {
    var item = initial
    val reset = { item = null }
    val name = item?.name
    if (name != null) return item.name
    reset()
    return ""
}
"#,
        )],
        &[],
    );
}

#[test]
fn a_member_val_read_from_a_member_function_proves_nothing() {
    assert_rejected_like_kotlinc(
        r#"
class Reader(val source: Source) {
    val selector = selectorFor(source)

    fun selected(): String = if (selector != null) source.select() else "plain"
}
"#,
    );
}
