fun box(): String {
    val left: Any = arrayListOf(1, 2)
    val right: Any = arrayListOf(1, 2)
    val other: Any = arrayListOf(1, 3)
    val shorter: Any = arrayListOf(1)
    if (left !== left) return "identity"
    if (left === right) return "aliased"
    if (left != right) return "eq"
    if (left == other) return "ne"
    if (left == shorter) return "length"
    if (left.hashCode() != right.hashCode()) return "hash differ"
    if (left.hashCode() != 994) return "hash ${left.hashCode()}"
    if ("$left" != "[1, 2]") return "text $left"
    if ("$other" != "[1, 3]") return "text $other"
    val nested: Any = arrayListOf(left, other)
    val nestedAgain: Any = arrayListOf(arrayListOf(1, 2), arrayListOf(1, 3))
    if (nested != nestedAgain) return "nested eq"
    if ("$nested" != "[[1, 2], [1, 3]]") return "nested $nested"
    val words: Any = arrayListOf("a", "b")
    val wordsAgain: Any = arrayListOf("a", "b")
    if (words != wordsAgain) return "string eq"
    if (words.hashCode() != 4066) return "string hash ${words.hashCode()}"
    if ("$words" != "[a, b]") return "string text $words"
    val holes: Any = arrayListOf<Int?>(null, 1)
    val holesAgain: Any = arrayListOf<Int?>(null, 1)
    if (holes != holesAgain) return "null eq"
    if (holes.hashCode() != 962) return "null hash ${holes.hashCode()}"
    if ("$holes" != "[null, 1]") return "null text $holes"
    val empty: Any = arrayListOf<Int>()
    val emptyAgain: Any = arrayListOf<Int>()
    if (empty != emptyAgain) return "empty eq"
    if (empty == left) return "empty ne"
    if (empty.hashCode() != 1) return "empty hash ${empty.hashCode()}"
    if ("$empty" != "[]") return "empty $empty"
    return "OK"
}
