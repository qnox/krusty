// `toString()` and string templates over a value whose static type erases its class (`Any?`, a
// type parameter, an interface) dispatch on what the value is at run time: `null`, a string, a
// primitive's box, `Unit`, an enum constant, an override, `kotlin.Any`'s own rendering, and a
// caught exception's `Throwable.toString`. Identity hashes differ between targets, so only the
// class-name part of `Any.toString` is compared. `Throwable.toString` names the class by a
// target-specific package (`java.lang` on the JVM, `kotlin` on wasm-js) or, on wasm-wasi, by its
// simple name alone, so only that simple name and the message are.
interface Shape

class Square : Shape {
    override fun toString(): String = "Square"
}

class Plain : Shape

enum class Color { RED, GREEN }

fun show(value: Any?): String = value.toString()

fun <T> template(value: T): String = "[$value]"

fun shape(value: Shape): String = "$value"

fun hasPrefix(text: String, prefix: String): Boolean {
    if (text.length < prefix.length) return false
    for (i in 0 until prefix.length) {
        if (text[i] != prefix[i]) return false
    }
    return true
}

fun hasSuffix(text: String, suffix: String): Boolean {
    val start = text.length - suffix.length
    if (start < 0) return false
    for (i in 0 until suffix.length) {
        if (text[start + i] != suffix[i]) return false
    }
    return true
}

fun box(): String {
    if (show("text") != "text") return "FAIL: string"
    if (show(null) != "null") return "FAIL: null"
    if (template(null) != "[null]") return "FAIL: null template"
    if (show(42) != "42" || template(-7) != "[-7]") return "FAIL: Int"
    if (show(3.toByte()) != "3" || show((-4).toShort()) != "-4") return "FAIL: Byte/Short"
    if (show(-9000000000L) != "-9000000000") return "FAIL: Long"
    if (show('c') != "c") return "FAIL: Char"
    if (show(true) != "true" || template(false) != "[false]") return "FAIL: Boolean"
    if (show(Unit) != "kotlin.Unit") return "FAIL: Unit"
    if (show(Color.GREEN) != "GREEN" || template(Color.RED) != "[RED]") return "FAIL: enum"
    if (show(Square()) != "Square" || shape(Square()) != "Square") return "FAIL: override"
    val plain = shape(Plain())
    if (!hasPrefix(plain, "Plain@") || plain.length <= 6) return "FAIL: Any.toString $plain"
    val failure = try {
        error("boom")
    } catch (e: IllegalStateException) {
        show(e)
    }
    if (!hasSuffix(failure, "IllegalStateException: boom")) return "FAIL: message $failure"
    val bare = try {
        throw IllegalArgumentException()
    } catch (e: IllegalArgumentException) {
        template(e)
    }
    if (!hasSuffix(bare, "IllegalArgumentException]")) return "FAIL: no message $bare"
    return "OK"
}
