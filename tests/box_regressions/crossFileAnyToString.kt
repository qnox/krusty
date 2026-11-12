// `Any.toString` and `Throwable.toString` dispatched in one file reach objects another file
// creates: local classes, anonymous objects, a class nested in a local class, and exception classes
// declared in executable code. Targets name such a class differently (the JVM by its binary name,
// Kotlin/Wasm by its package and enclosing classifiers), and print identity hashes differently, so
// only the class's own segment and the hash's presence are compared.
// FILE: show.kt
package p

fun show(value: Any?): String = value.toString()

fun base(value: Base): String = "$value"

fun hasSuffix(text: String, suffix: String): Boolean = endsAt(text, text.length, suffix)

/** Whether `suffix` ends at index `end` of `text`. */
fun endsAt(text: String, end: Int, suffix: String): Boolean {
    val start = end - suffix.length
    if (start < 0) return false
    for (i in 0 until suffix.length) {
        if (text[start + i] != suffix[i]) return false
    }
    return true
}

fun hasPrefix(text: String, prefix: String): Boolean {
    if (text.length < prefix.length) return false
    for (i in 0 until prefix.length) {
        if (text[i] != prefix[i]) return false
    }
    return true
}

/** Whether `text` is an identity rendering in package `p` whose class name ends with `name`. */
fun isIdentity(text: String, name: String): Boolean {
    if (!hasPrefix(text, "p.")) return false
    var at = -1
    for (i in 0 until text.length) {
        if (text[i] == '@') at = i
    }
    if (at < 0 || at == text.length - 1) return false
    if (!endsAt(text, at, name)) return false
    for (i in at + 1 until text.length) {
        val c = text[i]
        if (c != '-' && (c < '0' || c > '9') && (c < 'a' || c > 'f')) return false
    }
    return true
}

// FILE: main.kt
package p

open class Base

class Outer {
    fun local(): Any {
        class Made
        return Made()
    }

    fun anonymous(): Any = object {}
}

fun box(): String {
    class Local {
        inner class Deep
    }
    val instance = show(Local())
    if (!isIdentity(instance, "Local")) return "FAIL: local class $instance"
    val deep = show(Local().Deep())
    if (!isIdentity(deep, "Deep")) return "FAIL: nested in a local class $deep"
    val anonymous = show(object {})
    if (!isIdentity(anonymous, "")) return "FAIL: anonymous object $anonymous"
    val made = show(Outer().local())
    if (!isIdentity(made, "Made")) return "FAIL: local class of a member $made"
    val member = show(Outer().anonymous())
    if (!isIdentity(member, "")) return "FAIL: member object $member"
    val subclass = base(object : Base() {})
    if (!isIdentity(subclass, "")) return "FAIL: open class $subclass"

    class LocalFailure : IllegalStateException("local")
    val local = show(LocalFailure())
    if (!hasSuffix(local, "LocalFailure: local")) return "FAIL: local exception $local"
    val failure = show(object : Exception("anonymous") {})
    if (!hasSuffix(failure, ": anonymous")) return "FAIL: anonymous exception $failure"

    val hashed: Any = object {}
    if (hashed.hashCode() != hashed.hashCode()) return "FAIL: unstable identity hash"
    return "OK"
}
