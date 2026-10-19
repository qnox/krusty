package inner

class Outer<T> {
    inner class Inner<U>(val first: T, val second: U) {
        fun use(value: T, other: U) {}
    }
}
