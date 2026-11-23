// An interface member property initializes nothing, but its accessors still declare lambdas: each
// lambda spliced into a getter or setter body opens its inline-lambda frame, named after the class
// the lambda would compile to (`FieldSet$realFields$1`, `FieldSet$tagged$1$1`, `FieldSet$joined$2`).
interface FieldSet {
    val fields: List<String>
    var last: String

    val realFields: List<String>
        get() {
            val unrolled = ArrayList<String>(fields.size)
            fields.forEach { unrolled.add(it + "!") }
            return unrolled
        }

    val tagged: List<String>
        get() {
            val tags = fields.map { "<$it>" }
            tags.also { tagged -> tagged.forEach { last = it } }
            return tags
        }

    var joined: String
        get() = fields.fold("") { acc, field -> acc + field }
        set(value) {
            fields.forEach { last = value + it }
        }
}

class Fields(override val fields: List<String>) : FieldSet {
    override var last: String = ""
}

fun box(): String {
    val set = Fields(listOf("O", "K"))
    if (set.realFields != listOf("O!", "K!")) return "FAIL realFields: " + set.realFields
    if (set.tagged != listOf("<O>", "<K>") || set.last != "<K>") return "FAIL tagged: " + set.tagged
    set.joined = "OK"
    if (set.last != "OKK") return "FAIL joined: " + set.last
    return set.joined
}
