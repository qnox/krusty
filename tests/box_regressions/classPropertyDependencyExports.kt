// MODULE: lib
class ExportedProperties(val fixed: Int, var plain: Int) {
    private var saved: Int = 2
    var custom: Int
        get() = saved * 2
        set(value) { saved = value / 2 }
    val computed: Int get() = fixed + plain
}

fun libraryProperties(): ExportedProperties = ExportedProperties(7, 3)

// MODULE: main(lib)
fun box(): String {
    val value = libraryProperties()
    if (value.fixed != 7) return "constructor val"
    if (value.plain != 3) return "default getter"
    value.plain = 5
    if (value.plain != 5) return "default setter"
    if (value.custom != 4) return "custom getter"
    value.custom = 40
    if (value.custom != 40) return "custom setter"
    if (value.computed != 12) return "computed getter"
    return "OK"
}
