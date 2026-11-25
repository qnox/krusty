// WITH_STDLIB
// A source enum's `entries` and a library enum with state reach `kotlin.enums.enumEntries` in one
// program. The support unit that lowers `enumEntries` also builds `CharCategory`'s list, through
// the function it lowers, not through an export no earlier unit published.
enum class Phase { DAWN, DUSK }

fun box(): String {
    if (Phase.entries.size != 2 || Phase.entries[1] !== Phase.DUSK) return "entries"
    if ('a'.category != CharCategory.LOWERCASE_LETTER) return "category"
    if (CharCategory.entries.size != 30) return "library entries ${CharCategory.entries.size}"
    return "OK"
}
