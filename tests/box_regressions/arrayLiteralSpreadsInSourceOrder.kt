class Tag(val text: String)

var trace = ""

fun mark(text: String): Tag {
    trace += text
    return Tag(text)
}

fun third(vararg items: Tag): Tag = items[2]

fun box(): String {
    val head = arrayOf(Tag("a"), Tag("b"))
    val empty = arrayOf<Tag>()
    val tail = arrayOf(Tag("d"))

    val joined = arrayOf(*head, mark("c"), *empty, *tail)
    if (joined[0].text + joined[1].text + joined[2].text + joined[3].text != "abcd") return "joined"
    if (third(*head, Tag("x")).text != "x") return "vararg"

    // The spread array's elements are taken when it is reached.
    val snapshot = arrayOf(*head, run { head[0] = Tag("z"); mark("e") })
    if (snapshot[0].text != "a" || head[0].text != "z") return "snapshot"

    val copy = arrayOf(*tail)
    if (copy === tail || copy[0] !== tail[0]) return "copy"
    if (trace != "ce") return "order: $trace"
    return "OK"
}
