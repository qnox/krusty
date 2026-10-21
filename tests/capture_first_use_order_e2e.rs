//! A lifted lambda or local function takes its captures in the order its body first uses them,
//! receivers among the captured values: `prefix + count` in a member captures `this` first,
//! `count + prefix` captures `count` first.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

const SOURCE: &str = r#"
suspend fun step(s: String): String = s
fun <T> launchIt(block: suspend (Int) -> T): Any = block
fun <T> plainIt(block: (Int) -> T): Any = block
class Spec(private val prefix: String) {
    fun receiverFirst(count: Int) = launchIt { n -> step(prefix + n + count) }
    fun valueFirst(count: Int) = launchIt { n -> step("" + count + n + prefix) }
    fun between(count: Int, other: String) = launchIt { n -> step(other + prefix + n + count) }
    fun plain(count: Int) = plainIt { n -> prefix + n + count }
    fun local(count: Int): String {
        fun loc(n: Int) = prefix + n + count
        return loc(1)
    }
    fun shared(count: Int): String {
        var total = count
        fun bump(n: Int) { total += n + prefix.length }
        bump(1)
        val r = plainIt { n -> prefix + total + n }
        return r.toString() + total
    }
}
"#;

/// The indy lambdas and local functions lifted into the class, and their call sites.
#[test]
fn lifted_callables_take_the_receiver_where_the_body_first_uses_it() {
    byte_identical("capture_first_use_order", SOURCE, "Spec");
}

/// A suspend lambda's class declares its captures, and its constructor takes them, in that order.
#[test]
fn suspend_lambda_classes_take_the_receiver_where_the_body_first_uses_it() {
    for class in [
        "Spec$receiverFirst$1",
        "Spec$valueFirst$1",
        "Spec$between$1",
    ] {
        byte_identical("capture_first_use_order", SOURCE, class);
    }
}

const TWO_RECEIVERS: &str = r#"
fun <T> plainIt(block: () -> T): T = block()
class Receivers(private val prefix: String) {
    fun String.condition(count: Int): String {
        fun loc() = if (length > 0) "" + count + prefix else ""
        return loc()
    }
    fun String.member(count: Int): String {
        fun loc() = if (prefix.length > 0) "" + count + length else ""
        return loc()
    }
    fun String.lambda(count: Int): String {
        val f = { if (length > 0) "" + count + prefix else "" }
        return f()
    }
}
"#;

/// Two captured receivers are ordered between themselves by first use too: a condition the body
/// reads before its branch puts the extension receiver ahead of `count` and `this`.
#[test]
fn lifted_callables_order_two_receivers_by_first_use() {
    byte_identical(
        "capture_first_use_two_receivers",
        TWO_RECEIVERS,
        "Receivers",
    );
}

const NESTED_ONLY: &str = r#"
fun <T> plainIt(block: () -> T): T = block()
class Nested(private val prefix: String) {
    fun receiverFirst(count: Int): String {
        fun loc() = plainIt { prefix + count }
        return loc()
    }
    fun valueFirst(count: Int): String {
        fun loc() = plainIt { count.toString() + prefix }
        return loc()
    }
    fun partly(count: Int): String {
        fun loc() = plainIt { prefix } + count
        return loc()
    }
}
"#;

/// A local function that reaches its captures only through a nested lambda takes them in the
/// lambda's order.
#[test]
fn local_functions_order_captures_read_through_a_nested_lambda() {
    byte_identical("capture_first_use_nested_only", NESTED_ONLY, "Nested");
}

const MET_CLOSURES: &str = r#"
fun <T> plainIt(block: () -> T): T = block()
interface Sup { fun get(): String }
class Met(private val prefix: String) {
    fun objectFirst(count: Int): String {
        fun r() = prefix
        fun loc(): String {
            val o = object : Sup { override fun get() = "" + count }
            return o.get() + r()
        }
        return loc()
    }
    fun objectAfter(count: Int, other: Long): String {
        fun loc(): String {
            val x = other
            val o = object : Sup { override fun get() = prefix + count }
            return o.get() + x
        }
        return loc()
    }
    fun sibling(count: Int): String {
        fun a() = prefix + count
        fun b() = a()
        return b()
    }
    fun twoNested(count: Int, other: Long): String {
        fun loc() = plainIt { "" + other + prefix } + plainIt { "" + count + other }
        return loc()
    }
    fun ownAfter(count: Int, other: Long): String {
        fun loc() = plainIt { prefix + count } + other
        return loc()
    }
    fun callFirst(count: Int, other: Long): String {
        fun a() = prefix + count
        fun loc() = a() + other
        return loc()
    }
    fun lambdaOuter(count: Int, other: Long): String {
        val f = { plainIt { "" + count + prefix } + other }
        return f()
    }
}
"#;

/// After its own reads, a callable takes the captures of each nested lambda, called local function
/// and created anonymous object in that callee's order, in the order it meets them.
#[test]
fn lifted_callables_take_met_closures_captures_after_their_own() {
    byte_identical("capture_first_use_met_closures", MET_CLOSURES, "Met");
}

const SIBLING_LOCALS: &str = r#"
fun <T> plainIt(block: () -> T): T = block()
class Distinct(private val prefix: String) {
    fun test(count: Int): String {
        fun a(): String {
            val unused = 0
            val own = "A"
            return plainIt { own + prefix }
        }
        fun b(): String {
            val first = 5L
            val own = "B"
            fun loc(): String = a() + plainIt { "" + first + own + count }
            return loc()
        }
        return b()
    }
}
"#;

/// Sibling bodies number their locals afresh: `a`'s own local, which `a`'s lambda captures, is not
/// `b`'s local in the same slot, so it does not move `b`'s capture ahead of `this`.
#[test]
fn sibling_bodies_locals_stay_distinct_captures() {
    byte_identical(
        "capture_first_use_sibling_locals",
        SIBLING_LOCALS,
        "Distinct",
    );
}

const OBJECT_BEFORE_CALL: &str = r#"
class Anon(private val prefix: String) {
    fun test(count: Int): String {
        fun a(): String = prefix
        fun loc(): String {
            val obj = object { fun get(): Int = count }
            return obj.get().toString() + a()
        }
        return loc()
    }
}
"#;

/// An anonymous object met before a local call contributes its captures first.
#[test]
fn an_anonymous_object_contributes_its_captures_where_it_is_met() {
    byte_identical(
        "capture_first_use_object_before_call",
        OBJECT_BEFORE_CALL,
        "Anon",
    );
}
