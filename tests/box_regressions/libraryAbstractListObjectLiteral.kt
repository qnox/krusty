// `listOf` with several elements wraps its array through the stdlib's `asList`, an object literal
// extending the generic library class `AbstractList`. Every target materializes that class and
// dispatches the literal's overrides and the inherited members through it.
fun box(): String {
    val letters = listOf("O", "x", "K")
    if (letters.size != 3) return "size ${letters.size}"
    if (letters.indexOf("K") != 2) return "indexOf"
    if (!letters.contains("x")) return "contains"
    return letters[0] + letters[2]
}
