fun <T> identity(value: T): T = value
class Box<T : Comparable<T>>(val content: T)
val map: Map<String, List<Int>> = emptyMap()
