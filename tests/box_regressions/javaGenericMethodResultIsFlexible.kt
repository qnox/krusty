// TARGET_BACKEND: JVM
// FULL_JDK
// WITH_STDLIB
// PLATFORM-SPECIFIC: the result type is a Java method's type variable.

// A Java type variable is flexible where it is used as a type argument, so instantiating it keeps
// `E!`: the element of `Arrays.asList(t)` admits null even when `T` does not.
fun <T> replaceWithNull(t: T): T = java.util.Arrays.asList(t).set(0, null)

fun box(): String {
    if (replaceWithNull("b") != "b") return "Fail: set returns the previous element"
    return "OK"
}
