interface Sink<T> {
    fun put(value: T): T
}

class Holder<T>(val codec: Holder.Codec<T>) {
    interface Codec<T> {
        fun decode(text: String): String
        fun encode(value: T): T
    }
}

interface Adapter<T> {
    fun sink(): Sink<T> = object : Sink<T> {
        override fun put(value: T): T = value
    }

    fun boxed(): Holder<T> {
        val codec = object : Holder.Codec<T> {
            override fun decode(text: String): String = text
            override fun encode(value: T): T = value
        }
        return Holder(codec)
    }

    fun <R> local(value: T, tag: R): T {
        class Pair(val first: T, val second: R) {
            fun pick(): T = first
        }
        return Pair(value, tag).pick()
    }
}

object Text : Adapter<String>

fun box(): String {
    if (Text.sink().put("a") != "a") return "sink"
    val codec = Text.boxed().codec
    if (codec.encode(codec.decode("b")) != "b") return "codec"
    if (Text.local("c", 1) != "c") return "local"
    return "OK"
}
