// The order of two strings, read through `String.compareTo` and through `Comparable<String>`,
// and of the other builtin classes' values read through `Comparable`.

fun direct(a: String, b: String): Int = a.compareTo(b)

fun <T : Comparable<T>> generic(a: T, b: T): Int = a.compareTo(b)

fun <T : Comparable<T>> sign(a: T, b: T): Int = generic(a, b).let { if (it < 0) -1 else if (it > 0) 1 else 0 }

fun throughComparable(value: Comparable<String>, other: String): Int = value.compareTo(other)

fun box(): String {
    val apple = "app" + "le"
    val apply = "app" + "ly"
    val app = "a" + "pp"
    if (direct(apple, apply) != 'e' - 'y') return "fail direct difference: ${direct(apple, apply)}"
    if (direct(apply, apple) != 'y' - 'e') return "fail direct reversed"
    if (direct(app, apple) != -2) return "fail prefix: ${direct(app, apple)}"
    if (direct(apple, app) != 2) return "fail longer"
    if (direct(apple, "apple") != 0) return "fail equal"
    if (direct("", app) != -3) return "fail empty"
    if (!(apple < apply) || apple > apply || !(app <= apple)) return "fail operators"
    if (generic(apple, apply) != 'e' - 'y') return "fail generic"
    if (throughComparable(apply, apple) != 'y' - 'e') return "fail comparable"
    if (maxOf(apple, apply) != apply || minOf(apple, app) != app) return "fail maxOf/minOf"
    if (sign(3, 5) != -1 || sign(5L, 3L) != 1 || sign('a', 'c') != -1) return "fail integral"
    if (sign(true, false) != 1 || sign(1.toByte(), 1.toByte()) != 0) return "fail boolean/byte"
    if (sign((-2).toShort(), 2.toShort()) != -1) return "fail short"
    if (sign(-0.0, 0.0) != -1 || sign(0.0f, -0.0f) != 1) return "fail signed zero"
    if (sign(Double.NaN, Double.POSITIVE_INFINITY) != 1 || sign(Float.NaN, Float.NaN) != 0) {
        return "fail NaN"
    }
    if (sign(1.5, 2.5) != -1 || sign(2.5f, 1.5f) != 1) return "fail floating"
    return "OK"
}
