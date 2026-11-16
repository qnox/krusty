// An inline function whose file numbers its own `$WhenMappings` tables, called from another file:
// the caller inlines the callee's `when` and keeps reading the declaring file's tables, which the
// declaring file's other `when` numbers first.
// FILE: Decl.kt

enum class Size { S, M, L }

fun before(s: Size): Int = when (s) { Size.L -> 1; else -> 2 }

inline fun label(s: Size): String = when (s) { Size.M -> "m"; Size.S -> "s"; Size.L -> "l" }
// FILE: Caller.kt

fun labels(): String = label(Size.S) + label(Size.L)

fun box(): String = if (labels() == "sl" && before(Size.L) == 1) "OK" else "FAIL ${labels()}"
