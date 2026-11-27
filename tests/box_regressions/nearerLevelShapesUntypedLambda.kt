// A lambda without declared parameters takes its inputs from the nearest scope-tower level with a
// candidate that shapes it. A farther level whose candidate would give the lambda other inputs is
// no ambiguity: an explicitly imported `foo((Int) -> String)` owns `foo { it }` beside a
// current-package `foo((String) -> String)`, and a current-package `bar((String) -> String)` owns
// `bar { it }` beside a star-imported `bar((Int) -> String)`.
// FILE: a.kt
package a

fun foo(f: (Int) -> String): String = f(1)

// FILE: b.kt
package b

fun bar(f: (Int) -> String): String = f(2)

// FILE: main.kt
package app

import a.foo
import b.*

fun foo(f: (String) -> String): String = f("s")

fun bar(f: (String) -> String): String = f("near")

fun box(): String {
    val first = foo { (it + 1).toString() }
    if (first != "2") return "fail foo: $first"
    val second = bar { it + it.length }
    if (second != "near4") return "fail bar: $second"
    return "OK"
}
