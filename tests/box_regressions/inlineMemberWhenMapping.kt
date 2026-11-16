// An inline member of a class of the same file, inlined into a top-level function: the inlined
// `when` reads the member's class's `$WhenMappings` tables, not the caller's.
enum class Color { RED, GREEN, BLUE }

class Palette(val fallback: Color) {
    fun own(c: Color): Int = when (c) { Color.BLUE -> 7; else -> 8 }
    inline fun shade(c: Color): String =
        when (c) { Color.GREEN -> "g"; Color.RED -> "r"; Color.BLUE -> "b" }
}

fun paint(p: Palette, c: Color): String = p.shade(c) + p.shade(p.fallback)

fun box(): String {
    val painted = paint(Palette(Color.BLUE), Color.GREEN)
    return if (painted == "gb") "OK" else "FAIL $painted"
}
