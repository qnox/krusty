// `In<A>` and `In<B>` join at `In<A & B>`, which kotlinc reifies as `In<Nothing>`.
import kotlin.reflect.KType
import kotlin.reflect.typeOf
class In<in T>
interface A
interface B
inline fun <reified K> select(x: K, y: K): KType = typeOf<K>()
fun box(): String {
    val described = select(In<A>(), In<B>())
    return if (described == typeOf<In<Nothing>>()) "OK" else "fail: $described"
}
