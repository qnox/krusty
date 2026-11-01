//! A member extension's type parameters are inferred from its receiver through the receiver's
//! supertypes, as an ordinary extension's are: `List<String>` binds the `T` of `Iterable<T>`.
//!
//! From Exposed's `QueryBuilder`, whose `fun <T> Iterable<T>.appendTo(transform: QueryBuilder.(T)
//! -> Unit)` is called on a `List<Expression<*>>` inside a `QueryBuilder` lambda.

use super::common;

const SUPERTYPE_RECEIVERS: &str = r#"interface Source<out T> { fun read(): T }
interface Channel<out T> : Source<T>
class Fixed(private val value: String) : Channel<String> { override fun read() = value }

class Builder {
    val text = StringBuilder()
    operator fun invoke(body: Builder.() -> Unit) = body()
    fun <T> Iterable<T>.appendTo(separator: String = ", ", transform: Builder.(T) -> Unit) {
        forEachIndexed { index, element ->
            if (index > 0) text.append(separator)
            transform(element)
        }
    }
    fun <T> Source<T>.take(): T = read()
    val <T> Collection<T>.firstOrSelf: T get() = first()
    operator fun String.unaryPlus() { text.append(this) }
}

fun box(): String {
    val builder = Builder()
    builder {
        listOf("a", "bc").appendTo { +it.uppercase() }
        +"|"
        setOf("d").appendTo { +it.length.toString() }
        +"|"
        +Fixed("e").take().plus(listOf("f").firstOrSelf)
    }
    val text = builder.text.toString()
    return if (text == "A, BC|1|ef") "OK" else text
}
"#;

#[test]
fn member_extension_type_parameters_bind_through_receiver_supertypes() {
    common::expect_box_same_as_kotlinc(SUPERTYPE_RECEIVERS, "MemberExtensionReceiverSupertype");
}
