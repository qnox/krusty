// A formal's classifier bound is part of the call's constraint system: the argument that fixes
// `E` also fixes `T` through `E : Key<T>?`, before `V : T?` contributes only a lower constraint.

open class Key<T : Any>(val raw: T)
open class Expr<T>(val label: String)
class Col<T>(label: String) : Expr<T>(label)

fun <T : Any, E : Key<T>?, V : T?> match(key: Expr<E>, value: Expr<V>): String = key.label + value.label
fun <T : Any, E : Key<T>, V : T?> matchNonNull(key: Expr<E>, value: Expr<V>): String = key.label + value.label
fun <T : Any, E : Key<T>?, V : T?> Col<E>.matchReceiver(value: Expr<V>): String = label + value.label

fun <T : Any, V : T?> concreteKey(key: Expr<Key<T>>, value: Expr<V>): String = match(key, value)
fun <T : Any, V : T?> concreteNonNullKey(key: Expr<Key<T>>, value: Expr<V>): String = matchNonNull(key, value)
fun <T : Any, V : T?, E : Key<T>?> boundKey(key: Col<E>, value: Expr<V>): String = match(key, value)
fun <T : Any, V : T?, E : Key<T>?> Expr<V>.receiverKey(key: Col<E>): String = key.matchReceiver(this)

fun box(): String {
    val key = Col<Key<String>>("k")
    val value = Expr<String?>("v")
    val results = concreteKey(key, value) + "," +
        concreteNonNullKey(key, value) + "," +
        boundKey(key, value) + "," +
        value.receiverKey(key) + "," +
        match(key, Expr<String>("s"))
    return if (results == "kv,kv,kv,kv,ks") "OK" else results
}
