package refused

open class Box<T> {
    fun get(): T? = null
}

class IntBox : Box<Int>()
