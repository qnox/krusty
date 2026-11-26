// MODULE: lib
interface Source<T> {
    val String.item: T
    var String.slot: T
}

fun <T> readItem(source: Source<T>, key: String): T = with(source) { key.item }

fun <T> readSlot(source: Source<T>, key: String): T = with(source) { key.slot }

fun <T> writeSlot(source: Source<T>, key: String, value: T) {
    with(source) { key.slot = value }
}

// MODULE: main(lib)
class Measured : Source<Int> {
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
