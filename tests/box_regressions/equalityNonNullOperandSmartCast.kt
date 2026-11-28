// `a == b` holds only when `a` equals a value of a non-null type, so `a` is non-null where the
// equality is known to hold, for locals, receivers and properties, on either side.
class Box(val text: String?)

fun String?.same(other: String): Int = if (this == other) length else -1

fun local(a: String?, b: String): Int = if (a == b) a.length else -1

fun reversed(a: String?, b: String): Int = if (b == a) a.length else -1

fun conjunction(a: String?, b: String, c: Boolean): Int = if (a == b && c) a.length else -1

fun property(box: Box, b: String): Int = if (box.text == b) box.text.length else -1

fun box(): String {
    if (local("abc", "abc") != 3) return "fail local"
    if (local(null, "abc") != -1) return "fail local null"
    if (reversed("abc", "abc") != 3) return "fail reversed"
    if (conjunction("ab", "ab", true) != 2) return "fail conjunction"
    if (conjunction("ab", "ab", false) != -1) return "fail conjunction false"
    if (property(Box("abcd"), "abcd") != 4) return "fail property"
    if (property(Box(null), "abcd") != -1) return "fail property null"
    val nullable: String? = "xy"
    if ("xy".same("xy") != 2) return "fail receiver"
    if (nullable.same("xy") != 2) return "fail receiver nullable"
    return "OK"
}
