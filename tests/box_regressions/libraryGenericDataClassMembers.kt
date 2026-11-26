// A generic library data class that a library body constructs (`to`): its constructor takes the
// call's type arguments, and its generated members test and hash components of a type parameter
// whose bound admits null.
fun box(): String {
    val pair = "O" to "K"
    val same = "O" to "K"
    if (pair != same) return "Fail equals"
    if (pair == "O" to "k") return "Fail unequal"
    if (pair.hashCode() != same.hashCode()) return "Fail hashCode"
    if (pair.hashCode() != "O".hashCode() * 31 + "K".hashCode()) return "Fail hash value"
    val absent: Pair<String?, Int?> = null to null
    if (absent.hashCode() != 0) return "Fail null hashCode: ${absent.hashCode()}"
    if (pair.toString() != "(O, K)") return "Fail toString: $pair"
    return pair.first + pair.second
}
