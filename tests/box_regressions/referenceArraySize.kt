// `Array.size` is the provider's `ArraySize` operation, read as a property or as the bound of a
// `for` loop over the array, on a concrete, a generic, and a vararg-parameter array.
class Tag(val text: String)

fun <T> sizeOf(values: Array<T>): Int = values.size

fun count(vararg tags: Tag): Int = tags.size

class Holder(vararg val tags: Tag) {
    fun render(): String {
        var result = ""
        for (tag in tags) result += tag.text
        return result + tags.size
    }
}

fun box(): String {
    val tags = arrayOf(Tag("O"), Tag("K"))
    if (tags.size != 2) return "concrete"
    if (sizeOf(tags) != 2 || sizeOf(arrayOf<String>()) != 0) return "generic"
    if (count() != 0 || count(*tags, Tag("x")) != 3) return "vararg"
    if (Holder(*tags).render() != "OK2") return "property"
    var joined = ""
    for (tag in tags) joined += tag.text
    return joined
}
