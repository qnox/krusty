// WITH_STDLIB
// An enum's `entries`, read in the file declaring it: one list, of every constant in order, that
// stays the same list across reads and answers the list operations.
import kotlin.enums.enumEntries

enum class Suit { CLUBS, DIAMONDS, HEARTS, SPADES }

@OptIn(ExperimentalStdlibApi::class)
fun box(): String {
    val entries = Suit.entries
    if (entries !== Suit.entries) return "Fail: one list"
    if (entries.size != 4) return "Fail: size ${entries.size}"
    if (entries[0] !== Suit.CLUBS || entries[3] !== Suit.SPADES) return "Fail: get"
    if (entries.indexOf(Suit.HEARTS) != 2) return "Fail: indexOf"
    if (!entries.contains(Suit.DIAMONDS)) return "Fail: contains"
    var names = ""
    for (suit in entries) names += suit.name[0]
    if (names != "CDHS") return "Fail: iteration $names"
    if (entries.toString() != "[CLUBS, DIAMONDS, HEARTS, SPADES]") return "Fail: toString $entries"
    if (enumEntries<Suit>() !== entries) return "Fail: enumEntries"
    return "OK"
}
