// TARGET_BACKEND: JVM
// FULL_JDK
// WITH_STDLIB
// PLATFORM-SPECIFIC: the result type is a Java collection of a Java method's type variable.

// A Java type variable used as a type argument is flexible at that occurrence, so a call that
// instantiates the variable returns `(Mutable)List<E!>!` and its elements admit null.
fun <E> setFirstToNull(e: E): Any? = java.util.List.of(e).toMutableList().also { it.set(0, null) }.first()

fun <T> replaceWithNull(t: T): T = java.util.Arrays.asList(t).set(0, null)

fun box(): String {
    if (setFirstToNull(0.toByte()) != null) return "Fail: byte"
    if (setFirstToNull("a") != null) return "Fail: String"
    if (replaceWithNull("b") != "b") return "Fail: set returns the previous element"
    return nullElementEqualsLiteral()
}

// The element read back through `also { }` stays platform typed, so a null element compared with a
// primitive literal is a plain comparison, not an unboxing of null.
fun <E> getNullAsPlatform(e: E) = java.util.List.of(e).toMutableList().also { it.set(0, null) }.first()

fun nullElementEqualsLiteral(): String {
    val value = getNullAsPlatform(true)
    if (value == true) return "Fail: boolean"
    val number = getNullAsPlatform(0)
    if (number == 0) return "Fail: int"
    return "OK"
}
