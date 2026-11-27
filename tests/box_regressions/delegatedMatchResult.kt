class Groups(private val list: List<MatchGroup?>) : MatchGroupCollection, Collection<MatchGroup?> by list {
    override fun get(index: Int): MatchGroup? = list[index]
}

class Fixed : MatchResult {
    override val range: IntRange = 0..1
    override val value: String = "ab"
    override val groups: MatchGroupCollection = Groups(listOf(MatchGroup("ab", 0..1), MatchGroup("a", 0..0)))
    override val groupValues: List<String> = listOf("ab", "a")
    override fun next(): MatchResult? = null
}

class Matched(m: MatchResult) : MatchResult by m

fun box(): String {
    val matched = Matched(Fixed())
    if (matched.value != "ab") return "value: ${matched.value}"
    if (matched.groupValues[1] != "a") return "groupValues: ${matched.groupValues}"
    if (matched.destructured.component1() != "a") return "destructured: ${matched.destructured.component1()}"
    return "OK"
}
