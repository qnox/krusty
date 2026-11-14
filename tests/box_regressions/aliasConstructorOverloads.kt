// A constructor call written through a type alias selects among the classifier's constructors
// like any overloaded call: the most specific applicable one wins, and the alias's fixed
// arguments and the call's own inference share one constraint system (kotlinc 2.4.21).
class Tagged<T>(val value: T, val tag: String) {
    constructor(value: T) : this(value, "any")
    constructor(value: T, count: Int) : this(value, "count")
}

typealias Named = Tagged<String>
typealias Same<T> = Tagged<T>

fun box(): String {
    val fixed = Named("a")
    val counted = Named("b", 2)
    val tagged = Named("c", "own")
    val open = Same(4)
    val openCounted = Same(5, 1)
    val tags = fixed.tag + "," + counted.tag + "," + tagged.tag + "," + open.tag + "," + openCounted.tag
    if (tags != "any,count,own,any,count") return "FAIL tags: $tags"
    if (open.value + openCounted.value != 9) return "FAIL values"
    return "OK"
}
