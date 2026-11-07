// A recursive call to a generic member extension binds `C := Holder<R>` while `R` stays open.
// The declared bound `C : Sink<R>` relates that same open formal on both sides and still holds.
interface Sink<in E>

class Holder<E> : Sink<E>

object Extensions {
    fun <R, C : Sink<R>> C.same(depth: Int): C {
        if (depth != 0) {
            val next = Holder<R>()
            next.same(0)
            next.same<R, Holder<R>>(0)
        }
        return this
    }

    fun result(): String {
        val value = Holder<String>()
        val returned = value.same<String, Holder<String>>(1)
        return if (returned === value) "OK" else "identity"
    }
}

fun box(): String = Extensions.result()
