package library

import java.io.Serializable

abstract class Bag : java.util.AbstractList<String>(), Serializable, Comparable<Bag>

interface Store : Map<String, List<Int>>

enum class Color { RED }

annotation class Mark

class Typed(val name: String, val label: String?, val count: Int, val tags: Iterable<*>)
