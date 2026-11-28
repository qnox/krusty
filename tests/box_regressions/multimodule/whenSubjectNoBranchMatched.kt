// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: the program relies on JVM classfile replacement. The JVM loader overlays
// `m1`'s `S` and `K` with `m12`'s while `m2` stays compiled against the older hierarchy, so a
// sealed interface gains a subclass after `m2` was compiled. Native links `m12`, `m2` and the
// transitive `m1` as distinct libraries, which declares `pkg/S` twice, and its module model has
// no replacement operation; a wasm link has none either.
// An exhaustive subject `when` without an `else`, compiled against a sealed interface that gains a
// subclass before the program runs, throws `NoWhenBranchMatchedException`. From the 2.5
// API (`NoWhenBranchMatchedExceptionWithMessage`, since API 2.5) the failure passes the subject to
// `kotlin.internal.throwNoWhenBranchMatchedException`, whose message names it; before, the
// exception has no message. The stdlib the program runs against is that API.
// MODULE: m1
// FILE: m1.kt
package pkg

sealed interface S
class K : S

// MODULE: m12
// FILE: m1.kt
package pkg

sealed interface S
class K : S
class K2 : S {
    override fun toString(): String = "K2"
}

// MODULE: m2(m1)
// FILE: m2.kt
package pkg

fun sealedValue(s: S): String = when (s) {
    is K -> "K"
}

fun boundStatement(s: S, out: Array<String>) {
    when (val subject = s) {
        is K -> out[0] = subject.toString()
    }
}

fun booleanValue(b: Boolean?): Int = when (b) {
    true -> 1
    false -> 2
    null -> 3
}

// MODULE: m3(m12, m2)
// FILE: box.kt
package pkg

fun failure(subject: String, run: () -> Unit): String? {
    try {
        run()
    } catch (e: Exception) {
        val expected = if (KotlinVersion.CURRENT.isAtLeast(2, 5)) "No branch matched for subject: $subject" else null
        return if (e.message == expected) null else "$subject: message ${e.message}, expected $expected"
    }
    return "$subject: no exception"
}

fun box(): String {
    if (sealedValue(K()) != "K") return "sealed K"
    if (booleanValue(null) != 3) return "boolean null"
    failure("K2") { sealedValue(K2()) }?.let { return it }
    failure("K2") { boundStatement(K2(), arrayOf("")) }?.let { return it }
    return "OK"
}
