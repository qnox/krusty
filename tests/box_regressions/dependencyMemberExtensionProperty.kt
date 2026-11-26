// MODULE: lib
interface Source {
    val String.item: Int
    var String.slot: Int
}

fun readItem(source: Source, key: String): Int = with(source) { key.item }

fun readSlot(source: Source, key: String): Int = with(source) { key.slot }

fun writeSlot(source: Source, key: String, value: Int) {
    with(source) { key.slot = value }
}

// MODULE: main(lib)
class Measured : Source {
    private var stored = 0

    override val String.item: Int
        get() = length

    override var String.slot: Int
        get() = stored + length
        set(value) {
            stored = value * 10
        }
}

fun box(): String {
    val source = Measured()
    if (readItem(source, "four") != 4) return "item ${readItem(source, "four")}"
    writeSlot(source, "ab", 3)
    if (readSlot(source, "ab") != 32) return "slot ${readSlot(source, "ab")}"
    return "OK"
}
