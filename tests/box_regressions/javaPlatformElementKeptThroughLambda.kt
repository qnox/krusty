// TARGET_BACKEND: JVM
// FULL_JDK
// WITH_STDLIB
// PLATFORM-SPECIFIC: the element type is a Java method's flexible type variable.

// The lambda of `also` runs on a receiver whose element type is the platform `E!`. Its body does not
// constrain the enclosing function's own type parameter, so the result keeps `MutableList<E!>` and
// the element read back stays platform typed: a null element compared with a literal is a plain
// comparison, not an unboxing of null.
fun <E> getNullAsPlatform(e: E) = java.util.List.of(e).toMutableList().also { it.set(0, null) }.first()

fun <E> keepsElementType(e: E) = java.util.List.of(e).toMutableList().also { it.add(e) }

fun box(): String {
    val value = getNullAsPlatform(true)
    if (value == true) return "Fail: boolean"
    val number = getNullAsPlatform(0)
    if (number == 0) return "Fail: int"
    val kept: MutableList<Boolean?> = keepsElementType(true)
    if (kept.size != 2) return "Fail: size"
    return "OK"
}
