// LANGUAGE: +ContextParameters +CallableReferencesToContextual
// KOTLINC_VERSION: [2.5.0,)
// WITH_STDLIB
import kotlin.jvm.JvmInline
// A function reference binds a nullable value-class context. An inline factory returns the
// reference, so its copies share one adapter. A non-null value reaches the declaration unboxed and
// `null` stays `null`; a nullable value class over a scalar stays boxed.

OPTIONAL_JVM_INLINE_ANNOTATION
value class Z(val value: String)

OPTIONAL_JVM_INLINE_ANNOTATION
value class N(val value: Int)

context(z: Z?)
fun nullable(): String = z?.value ?: "null"

context(n: N?)
fun scalar(): String = n?.value?.toString() ?: "null"

inline fun <T> factory(value: T, build: (T) -> () -> String): () -> String = build(value)

fun box(): String {
    val present = context<Z?, () -> String>(Z("OK")) { ::nullable }
    if (present() != "OK") return "FAIL 1: ${present()}"
    val absent = context<Z?, () -> String>(null) { ::nullable }
    if (absent() != "null") return "FAIL 2: ${absent()}"
    val inlinePresent = factory(Z("OK")) { z -> context<Z?, () -> String>(z) { ::nullable } }
    if (inlinePresent() != "OK") return "FAIL 3: ${inlinePresent()}"
    val inlineAbsent = factory<Z?>(null) { z -> context<Z?, () -> String>(z) { ::nullable } }
    if (inlineAbsent() != "null") return "FAIL 4: ${inlineAbsent()}"
    val boxedScalar = context<N?, () -> String>(N(7)) { ::scalar }
    if (boxedScalar() != "7") return "FAIL 5: ${boxedScalar()}"
    val boxedScalarNull = context<N?, () -> String>(null) { ::scalar }
    if (boxedScalarNull() != "null") return "FAIL 6: ${boxedScalarNull()}"
    return "OK"
}
