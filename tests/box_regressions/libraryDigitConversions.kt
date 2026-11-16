// The standard library converts a digit through its private table of digit ranges, a
// file-private top-level property whose initializer builds an array.
fun box(): String {
    if ("42".toInt() != 42) return "toInt"
    if ("-7".toIntOrNull() != -7) return "toIntOrNull"
    if ("x1".toIntOrNull() != null) return "not a number"
    if ('7'.digitToInt() != 7) return "digitToInt"
    if ('٣'.digitToInt() != 3) return "Arabic-Indic digit"
    if ('a'.digitToIntOrNull() != null) return "not a digit"
    if ("ff".toInt(16) != 255) return "radix"
    return "OK"
}
