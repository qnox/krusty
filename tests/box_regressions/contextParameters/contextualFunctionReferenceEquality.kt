// LANGUAGE: +ContextParameters +CallableReferencesToContextual
// TARGET_BACKEND: JVM

import kotlin.jvm.internal.CallableReference

context(a: String, b: Int)
fun foo(): String = a + b

class C(val x: String) {
    context(a: String)
    fun bar(): String = x + a
}

fun plain(): String = "plain"

fun contexts(reference: Any): List<Any?>? =
    (reference as CallableReference).boundContextArguments?.toList()

fun box(): String {
    val r: () -> String = context("A", 1) { ::foo }
    if (contexts(r) != listOf<Any?>("A", 1)) return "FAIL foo: ${contexts(r)}"
    if (r() != "A1") return "FAIL call: ${r()}"

    val c = C("X")
    val rb: () -> String = context("A") { c::bar }
    if (contexts(rb) != listOf<Any?>("A")) return "FAIL bar: ${contexts(rb)}"

    if (contexts(::plain) != null) return "FAIL plain: ${contexts(::plain)}"

    // Equal references bind equal context arguments.
    val same1: () -> String = context("A", 1) { ::foo }
    val same2: () -> String = context("A", 1) { ::foo }
    val other: () -> String = context("B", 1) { ::foo }
    if (same1 != same2) return "FAIL equal"
    if (same1.hashCode() != same2.hashCode()) return "FAIL hash"
    if (same1 == other) return "FAIL differ"
    return "OK"
}
