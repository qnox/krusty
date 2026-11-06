object Events {
    var text = ""
}

object Key {
    init { Events.text += "K" }
}

class Cells {
    var stored = ""
    var reads = 0
    var writes = 0

    operator fun get(key: Key): String {
        reads += 1
        return stored
    }

    operator fun set(key: Key, value: String) {
        writes += 1
        stored = value
    }
}

fun next(): String {
    Events.text += "R"
    return "OK"
}

fun box(): String {
    val cells = Cells()
    cells[Key] += next()
    return if (cells.stored == "OK" && cells.reads == 1 && cells.writes == 1 && Events.text == "KR")
        "OK" else Events.text + ":" + cells.stored + ":" + cells.reads + ":" + cells.writes
}
