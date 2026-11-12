// Two `typeOf` sites naming one type describe it equally and hash alike; a type differing in its
// classifier, an argument, a projection or its nullability is another type.
import kotlin.reflect.typeOf

class G<T>
interface A
interface B

inline fun <reified T> described() = typeOf<T>()

fun box(): String {
    val list = typeOf<List<String>>()
    if (list != described<List<String>>()) return "fail: one type at two sites"
    if (list.hashCode() != described<List<String>>().hashCode()) return "fail: hashCode"
    if (list == typeOf<List<String?>>()) return "fail: argument nullability"
    if (list == typeOf<List<String>?>()) return "fail: nullability"
    if (list == typeOf<MutableList<String>>()) return "fail: classifier"
    if (typeOf<G<in A>>() == typeOf<G<A>>()) return "fail: projection"
    if (typeOf<G<out A>>() == typeOf<G<in A>>()) return "fail: variance"
    if (typeOf<G<*>>() != described<G<*>>()) return "fail: star projection"
    if (typeOf<G<A>>() == typeOf<G<B>>()) return "fail: argument"
    if (typeOf<(A) -> B>() != typeOf<Function1<A, B>>()) return "fail: function type"
    if (typeOf<Unit>() != described<Unit>()) return "fail: Unit"
    val erased: Any = list
    if (!erased.equals(described<List<String>>())) return "fail: equals through Any"
    if (erased == "kotlin.collections.List<kotlin.String>") return "fail: equal to a string"
    return "OK"
}
