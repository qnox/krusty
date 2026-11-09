//! A lambda passed to a classpath inline function's `crossinline` parameter, which the function's
//! anonymous object invokes, captures the caller's receivers into the object kotlinc regenerates
//! at the call: the enclosing instance of a member as `this$0`, unnumbered and without the
//! `$inlined` suffix the copy gives its other captures, and an extension receiver as
//! `$this_<function>$inlined`. The lambda reads the caller's properties through their accessors,
//! since its code runs in the object, not in the caller's class.
//!
//! Two lambdas passed to one call that both capture the enclosing instance share one `this$0`
//! field: the copy's constructor takes the instance once per lambda and stores the first.
//!
//! The kotlinx `Flow.map { it + property }` shape inside a class reached the inliner with the
//! captured instance unnamed and was rejected after route planning gave it the call.

use super::common;

const LIB: &str = r#"
package lib

interface Source<out T> {
    fun next(): T
}

interface Pending<out T> {
    suspend fun next(): T
}

inline fun <T> source(crossinline produce: () -> T): Source<T> =
    object : Source<T> {
        override fun next(): T = produce()
    }

interface Keyed {
    fun key(v: Int): Int
}

inline fun pair(crossinline a: (Int) -> Int, crossinline b: (Int) -> Int): Keyed =
    object : Keyed {
        override fun key(v: Int) = a(v) + b(v)
    }

inline fun <T> pending(crossinline produce: suspend () -> T): Pending<T> =
    object : Pending<T> {
        override suspend fun next(): T = produce()
    }
"#;

const MAIN: &str = r#"
import lib.*

class Counter(val start: Int) {
    var step = 2

    fun scaled(s: Source<Int>) = source { s.next() * start + step }

    fun Int.shifted() = source { this + start }

    fun shiftedTen() = 10.shifted()

    fun later(s: Pending<Int>) = pending { s.next() + start }

    fun both() = pair({ it + start }, { it * step })
}
"#;

/// The copies and the calling class are kotlinc's, byte for byte.
#[test]
fn captured_caller_receivers_regenerate_the_object_like_kotlinc() {
    let classes = common::classes_against_kotlinc_lib("Receivers", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
