// WITH_STDLIB
// LANGUAGE: +ContextParameters
// FILE: lib.kt
context(s: String)
fun foo() = s

fun bar(f: () -> String) = f()

inline fun baz(crossinline f: () -> String) = bar { f() }

// FILE: main.kt
context(_: String)
fun qux() = baz { foo() }

fun box() = context("OK") { qux() }
