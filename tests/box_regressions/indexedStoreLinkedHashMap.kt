// A generic construction stored through an indexed `set` takes the type arguments its own
// arguments do not bind from the selected `set`'s value parameter.

class Binding<K, P>(val name: String, val value: P?)

class Registry {
    var stored: Binding<Any, Any?>? = null

    operator fun set(key: String, binding: Binding<Any, Any?>) {
        stored = binding
    }
}

fun box(): String {
    val byName = LinkedHashMap<String, Binding<Any, Any?>>()
    byName["map"] = Binding("O", "K")
    val fromMap = byName["map"]!!
    if (fromMap.name + fromMap.value != "OK") return "map: ${fromMap.name}${fromMap.value}"

    val registry = Registry()
    registry["member"] = Binding("O", "K")
    val fromMember = registry.stored!!
    return fromMember.name + fromMember.value
}
