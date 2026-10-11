//! A bare class literal of a generic classifier denotes the classifier star-projected over its
//! declared type parameters: `H::class` is `KClass<H<*>>` and `H::class.java` is `Class<H<*>>`,
//! never the raw `H`.

use super::common;

/// The declared `Class<H<*>>` accepts `H::class.java`, and `Holder::class.java` fills the
/// `Class<Holder<*>>` parameter of a Java factory. `box` leaves `Holder` uninitialized: the check
/// is that the factory call is typed as kotlinc types it.
#[test]
fn a_bare_generic_class_literal_is_star_projected() {
    common::expect_box_same_as_kotlinc(
        "\
import java.util.concurrent.atomic.AtomicReferenceFieldUpdater

class H<T>
class Bounded<T : CharSequence, U>
class Outer<T> { inner class Inner }

val declared: Class<H<*>> = H::class.java
val bounded: Class<Bounded<*, *>> = Bounded::class.java
val inner: Class<Outer<*>.Inner> = Outer.Inner::class.java

class Holder<T> {
    @Volatile private var s: String? = null

    companion object {
        val U: AtomicReferenceFieldUpdater<Holder<*>, String> =
            AtomicReferenceFieldUpdater.newUpdater(Holder::class.java, String::class.java, \"s\")
    }
}

fun box(): String {
    if (declared.simpleName != \"H\") return declared.simpleName
    if (bounded.simpleName != \"Bounded\") return bounded.simpleName
    if (inner.simpleName != \"Inner\") return inner.simpleName
    return \"OK\"
}
",
        "BareGenericClassLiteral",
    );
}

/// With no declared type, the property's type comes from signature solving: kotlinc publishes
/// `Class<G<*>>` and `KClass<G<*>>` in the getters' generic signatures.
#[test]
fn an_inferred_bare_generic_class_literal_is_star_projected() {
    common::assert_class_matches_kotlinc(
        "InferredGenericClassLiteral",
        "\
class G<T>

val java = G::class.java
val kotlin = G::class
",
        "InferredGenericClassLiteralKt",
    );
}
