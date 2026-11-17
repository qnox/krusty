// `kotlin.text.CharCategory` is a library enum with state: each constant keeps the `value` and
// `code` its constructor stores, `contains` is a member function, and the companion's
// `valueOf(Int)` finds a constant through `entries`. `Char.category` reaches the companion, and
// `lowercase` reaches the categories through `isCaseIgnorable`.
fun box(): String {
    if ('a'.category != CharCategory.LOWERCASE_LETTER) return "category ${'a'.category}"
    if ('7'.category.code != "Nd") return "code ${'7'.category.code}"
    if ('Q' !in CharCategory.UPPERCASE_LETTER) return "contains"
    if ('q' in CharCategory.UPPERCASE_LETTER) return "contains lower"
    if ("Ä'B".lowercase() != "ä'b") return "lowercase"
    return "OK"
}
