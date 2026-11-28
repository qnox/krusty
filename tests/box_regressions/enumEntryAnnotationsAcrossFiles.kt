// An enum's entries carry annotations whose classes and argument constants other files declare,
// and a third file reads the entries. Each entry's applications are bound and folded once, under
// the entry's own scope, with their arguments kept for the file that publishes the entries.
// FILE: tags.kt
const val PREFIX = "o"

annotation class Tag(val value: String)

annotation class Weight(val grams: Int)

// FILE: colour.kt
enum class Colour(val code: String) {
    @Tag(PREFIX + "k")
    OK("O"),

    @Tag("x")
    @Weight(2 * 21)
    KO("K"),
}

// FILE: main.kt
fun box(): String {
    if (Colour.entries.size != 2) return "Fail entries: ${Colour.entries.size}"
    if (Colour.KO.ordinal != 1) return "Fail ordinal"
    return Colour.OK.code + Colour.valueOf("KO").code
}
