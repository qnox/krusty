// A generic construction stored through an indexed `set` takes the type arguments its own
// arguments do not bind from the selected `set`'s value parameter, whether that parameter is a
// class type argument or written directly.

class Binding<K, P>(val name: String, val value: P?)

class Slot<K, V> {
    private var key: K? = null
    private var value: V? = null

    operator fun set(key: K, value: V) {
        this.key = key
        this.value = value
    }

    operator fun get(key: K): V? = if (key == this.key) value else null
}

class Registry {
    var stored: Binding<Any, Any?>? = null

    operator fun set(key: String, binding: Binding<Any, Any?>) {
        stored = binding
    }
}

fun box(): String {
    val byName = Slot<String, Binding<Any, Any?>>()
    byName["map"] = Binding("O", "K")
    val fromMap = byName["map"]!!
    if (fromMap.name != "O" || fromMap.value != "K") return "map: ${fromMap.name}"

    val registry = Registry()
    registry["member"] = Binding("O", "K")
    val fromMember = registry.stored!!
    if (fromMember.name != "O" || fromMember.value != "K") return "member: ${fromMember.name}"
    return "OK"
}
