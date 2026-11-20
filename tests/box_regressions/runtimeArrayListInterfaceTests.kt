// WITH_STDLIB

fun box(): String {
    val values = ArrayList<String>()
    values.add("OK")
    val list: Any = values
    if (list !is Collection<*>) return "is Collection"
    if (list !is List<*>) return "is List"
    if (list !is Iterable<*>) return "is Iterable"
    if (list is Map<*, *>) return "is Map"
    val collection = list as Collection<*>
    if (collection.size != 1) return "size"
    return (list as List<*>)[0] as String
}
