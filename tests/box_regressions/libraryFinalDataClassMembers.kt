// A final library data class: its constructor, members and `Any` overrides come from the library.
fun box(): String {
    val group = MatchGroup("OK", 0..1)
    val same = MatchGroup("OK", 0..1)
    if (group != same) return "Fail equals"
    if (group == MatchGroup("OK", 0..2)) return "Fail unequal"
    if (group.hashCode() != same.hashCode()) return "Fail hashCode"
    if (group.toString() != "MatchGroup(value=OK, range=0..1)") return "Fail toString: $group"
    val (value, range) = group
    if (range.last != 1) return "Fail range"
    return value
}
