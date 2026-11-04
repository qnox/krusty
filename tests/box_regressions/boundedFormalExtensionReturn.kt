// Each body type-checks only when the extension's result names the caller's variables.
class Box<T>(val label: String)
class Pair2<A, B>(val first: A, val second: B)
operator fun <A : Number?, B : A> Box<A>.plus(t: B): Pair2<A, B> = Pair2(t, t)
fun <A : Number?, B : A> Box<A>.join(t: B): Pair2<A, B> = Pair2(t, t)
fun <X : Number?, Y : X> viaOperator(b: Box<X>, t: Y): Pair2<X, Y> = b + t
fun <X : Number?, Y : X> viaCall(b: Box<X>, t: Y): Pair2<X, Y> = b.join(t)
fun <X : Number, Y : X> viaNonNull(b: Box<X>, t: Y): Pair2<X, Y> = b + t

fun box(): String = "OK"
