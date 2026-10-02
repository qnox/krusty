// Kotlin's answers for `map_iterator_identity.c`: the class of each iterator a map, a set or one of
// a map's views hands out -- its qualified and simple names and its superclass's -- and whether an
// `is Iterator<*>` holds, for every kind the runtime makes.
fun box(): String = buildString {
    fun show(label: String, iterator: Iterator<*>) {
        val any: Any = iterator
        appendLine("$label: ${any::class.qualifiedName} ${any::class.simpleName} " +
            "super=${any.javaClass.superclass.kotlin.qualifiedName} ${any is Iterator<*>}")
    }
    val hash = hashMapOf<Any, Any>(1 to "a")
    val linked = mutableMapOf<Any, Any>(1 to "a")
    show("HashMap", hash.iterator())
    show("HashMap.keys", hash.keys.iterator())
    show("HashMap.values", hash.values.iterator())
    show("HashMap.entries", hash.entries.iterator())
    show("LinkedHashMap", linked.iterator())
    show("LinkedHashMap.keys", linked.keys.iterator())
    show("LinkedHashMap.values", linked.values.iterator())
    show("LinkedHashMap.entries", linked.entries.iterator())
    show("HashSet", hashSetOf<Any>(1).iterator())
    show("LinkedHashSet", mutableSetOf<Any>(1).iterator())
    show("empty HashMap.keys", HashMap<Any, Any>().keys.iterator())
}
