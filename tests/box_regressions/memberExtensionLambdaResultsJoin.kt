// A member extension's formal solved from two lambda results takes their common supertype, as a
// top-level extension's does: `{ empty() }` returns `Out<Nothing>` and `{ Full(it) }` returns
// `Out<T>`, so `fold`'s `T` is `Out<T>` rather than the first lambda fixing it to `Out<Nothing>`.
// The extension receiver is held in a companion object and in an ordinary class (mirai's `Either`).

open class Out<out T>

class Empty : Out<Nothing>()

class Full<T>(val value: T) : Out<T>()

fun <T> empty(): Out<T> = Empty()

class Either<out L : Any, out R>(val left: L?, val right: R)

class Folds {
    fun <L : Any, R, T> Either<L, R>.fold(onLeft: (L) -> T, onRight: (R) -> T): T {
        val left = left
        return if (left != null) onLeft(left) else onRight(right)
    }

    fun <T> Either<Throwable, T>.toOut(): Out<T> = fold(onLeft = { empty() }, onRight = { Full(it) })

    companion object {
        fun <L : Any, R, T> Either<L, R>.foldBoth(onLeft: (L) -> T, onRight: (R) -> T): T {
            val left = left
            return if (left != null) onLeft(left) else onRight(right)
        }

        fun <T> Either<Throwable, T>.toOut(): Out<T> = foldBoth({ empty() }, { Full(it) })
    }
}

fun read(out: Out<String>): String = if (out is Full<String>) out.value else "empty"

fun box(): String {
    val right = Either<Throwable, String>(null, "O")
    val left = Either<Throwable, String>(IllegalStateException(), "unused")
    val member = with(Folds()) { read(right.toOut()) + read(left.toOut()) }
    if (member != "Oempty") return "member: $member"
    val companion = with(Folds.Companion) { read(Either<Throwable, String>(null, "K").toOut()) }
    if (companion != "K") return "companion: $companion"
    return "OK"
}
