// TARGET_BACKEND: JVM
// FULL_JDK
// WITH_STDLIB
// PLATFORM-SPECIFIC: the referenced callables are Java static methods.

// The reference's input is the call's own type variable `T`, which the reference does not fix:
// the result `List<Int>` fixes the Java method's `E = Int`, and `T` is bounded by it afterwards.
fun <T> accept(reference: (T) -> List<Int>): String = "OK"

fun box(): String {
    if (accept(java.util.Collections::singletonList) != "OK") return "Fail: singletonList"
    if (accept(java.util.Arrays::asList) != "OK") return "Fail: asList"
    return "OK"
}
