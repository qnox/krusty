// An enum's `entries` is the stdlib's `EnumEntriesList`, which extends `AbstractList`. Calls
// through `EnumEntries`, whose members are fake overrides of `List`'s, reach `containsAll` and
// `isEmpty`, which the list inherits from `AbstractCollection` as `AbstractList` does. Every class
// that implements `List` this way, `AbstractList`'s own sublist among them, dispatches the
// `List` member to the inherited body.
enum class Level { LOW, MIDDLE, HIGH }

fun box(): String {
    val levels = Level.entries
    if (levels.isEmpty()) return "isEmpty"
    if (!levels.containsAll(levels)) return "containsAll"
    if (!levels.contains(Level.HIGH)) return "contains"
    return "OK"
}
