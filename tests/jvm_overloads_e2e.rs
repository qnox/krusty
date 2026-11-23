//! `@JvmOverloads` on a function or constructor with defaults: kotlinc's JVM backend adds one
//! overload per trailing run of omitted defaults, `for i in defaults - 1 downTo 0`, each keeping the
//! parameters without a default and the first `i` with one. An overload is `final`, copies the
//! declaration's annotations, type parameters and visibility, checks its own non-null reference
//! parameters, and calls the declaration's `$default` wrapper with zero placeholders, the mask and a
//! null marker. kotlinc appends the overloads to the class after every member its frontend
//! declared, ahead of the members a later lowering adds; a companion's `@JvmStatic` overloads also
//! get the outer class's static bridges. Each class is compared byte for byte with kotlinc's.

use super::common;

const FUNCTIONS: &str = "package p

@JvmOverloads fun top(a: Int, b: String = \"b\", c: Long = 3L): String = \"$a$b$c\"

@JvmOverloads fun String.ext(n: Int = 1, flag: Boolean = false) = repeat(n) + flag

class C {
    @JvmOverloads fun <T> m(t: T, x: Int = 1, y: List<T>? = null): T = t
    @JvmOverloads internal fun hidden(d: Double = 1.0) = d
    fun plain() = 1
    @JvmOverloads fun middle(a: String = \"a\", b: Int, c: Char = 'c') = a + b + c
}

object O {
    @JvmOverloads fun o(s: String, t: String = s) = s + t
    @JvmOverloads @JvmStatic fun st(s: String = \"\", n: Int = 2) = s + n
}
";

#[test]
fn function_overloads_match_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "JvmOverloadsFunctions",
        FUNCTIONS,
        &["p/JvmOverloadsFunctionsKt", "p/C", "p/O"],
    );
}

const CONSTRUCTORS: &str = "package p

class Primary @JvmOverloads constructor(val a: Int, val b: Int = 2, val c: String = \"c\")

class AllDefaults @JvmOverloads constructor(val a: Int = 1, val b: String = \"b\")

class Secondary(val s: String) {
    @JvmOverloads constructor(a: Int, b: String = \"b\", c: Int = 3) : this(\"$a$b$c\")
    fun f() = s
}

class Outer {
    inner class Inner @JvmOverloads constructor(val x: Int = 1, val y: String = \"y\")
}

open class Generic<T> @JvmOverloads constructor(val t: T, val list: List<T> = emptyList())
";

#[test]
fn constructor_overloads_match_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "JvmOverloadsConstructors",
        CONSTRUCTORS,
        &[
            "p/Primary",
            "p/AllDefaults",
            "p/Secondary",
            "p/Outer$Inner",
            "p/Generic",
        ],
    );
}

const RESTRICTED_CONSTRUCTORS: &str = "package p

class PrivateSecondary private constructor(val x: Int) {
    @JvmOverloads private constructor(a: String, b: Int = 1, c: Int = 2) : this(b)
}

class ProtectedSecondary(val x: Int) {
    @JvmOverloads protected constructor(a: String, b: Int = 1) : this(b)
}
";

#[test]
fn private_and_protected_constructor_overloads_keep_their_access_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "JvmOverloadsRestrictedConstructors",
        RESTRICTED_CONSTRUCTORS,
        &["p/PrivateSecondary", "p/ProtectedSecondary"],
    );
}

const SEALED: &str = "package p

sealed class SealedPrimary @JvmOverloads constructor(val x: Int = 1, val y: Int = 2) {
    class Leaf : SealedPrimary()
}

sealed class SealedMixed @JvmOverloads constructor(val x: Int = 1, val y: Int = 2) {
    @JvmOverloads protected constructor(a: String, b: Int = 3, c: Int = 4) : this(b)
    protected constructor(z: Long) : this(1)
    class Leaf : SealedMixed()
}
";

#[test]
fn sealed_constructor_overloads_get_their_marker_accessors_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "JvmOverloadsSealed",
        SEALED,
        &[
            "p/SealedPrimary",
            "p/SealedPrimary$Leaf",
            "p/SealedMixed",
            "p/SealedMixed$Leaf",
        ],
    );
}

const ENUMS: &str = "package p

enum class WithSecondary(val n: Int) {
    X(1), Y(\"s\");
    @JvmOverloads constructor(s: String, a: Int = 3, b: Int = 4) : this(a + b)
}
";

#[test]
fn enum_constructor_overloads_keep_the_name_and_ordinal_prefix_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc("JvmOverloadsEnums", ENUMS, &["p/WithSecondary"]);
}

// PLATFORM-SPECIFIC: kotlinc moves the annotations of a constructor hidden behind a value-class
// marker accessor onto that accessor, which krusty does not model for any such constructor yet, so
// the class files are compared by behaviour here and not byte for byte.
const VALUE_CLASS_CONSTRUCTORS: &str = "@JvmInline value class V(val v: Int)

class HiddenSecondary(val x: String) {
    @JvmOverloads constructor(a: V, b: Int = 1, c: Int = 2) : this(\"${a.v}$b$c\")
}

class HiddenPrimary @JvmOverloads constructor(val v: V, val a: Int = 1, val b: String = \"b\") {
    override fun toString() = \"${v.v}$a$b\"
}

fun box(): String {
    val int = Int::class.javaPrimitiveType
    val marker = Class.forName(\"kotlin.jvm.internal.DefaultConstructorMarker\")
    val secondary = HiddenSecondary::class.java
    val one = secondary.getDeclaredConstructor(int)
    one.isAccessible = true
    if ((one.newInstance(7) as HiddenSecondary).x != \"712\") return \"secondary: ${(one.newInstance(7) as HiddenSecondary).x}\"
    val accessor = secondary.getConstructor(int, marker)
    if ((accessor.newInstance(8, null) as HiddenSecondary).x != \"812\") return \"secondary accessor\"
    val primary = HiddenPrimary::class.java
    val first = primary.getDeclaredConstructor(int)
    first.isAccessible = true
    if (first.newInstance(5).toString() != \"51b\") return \"primary: ${first.newInstance(5)}\"
    val hidden = primary.getConstructor(int, marker)
    if (hidden.newInstance(6, null).toString() != \"61b\") return \"primary accessor\"
    return \"OK\"
}
";

#[test]
fn value_class_constructor_overloads_are_reachable_through_marker_accessors() {
    common::expect_box_same_as_kotlinc(VALUE_CLASS_CONSTRUCTORS, "jvm_overloads_value_class");
}

const COMPANION: &str = "package p

class Host {
    companion object {
        @JvmOverloads @JvmStatic fun s(a: String, b: Boolean = false) = a + b
        @JvmOverloads fun plain(x: Int = 0) = x
    }
    fun after() = { 1 }
}
";

#[test]
fn companion_overloads_and_static_bridges_match_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "JvmOverloadsCompanion",
        COMPANION,
        &["p/Host", "p/Host$Companion"],
    );
}

/// Java code and reflection reach every overload, which fills the omitted defaults.
const CALLED: &str =
    "class C @JvmOverloads constructor(val a: Int, val b: Int = 2, val c: String = \"c\") {
    @JvmOverloads fun m(x: String, y: Int = 1, z: String = \"z\") = x + y + z
    override fun toString() = \"$a$b$c\"
}

object T {
    @JvmOverloads @JvmStatic fun top(a: Int = 4, b: String = \"t\") = \"$a$b\"
}

fun box(): String {
    val c = C::class.java.getConstructor(Int::class.javaPrimitiveType).newInstance(1)
    if (c.toString() != \"12c\") return \"ctor: $c\"
    val m = C::class.java.getMethod(\"m\", String::class.java, Int::class.javaPrimitiveType)
    val r = m.invoke(c, \"x\", 5)
    if (r != \"x5z\") return \"m: $r\"
    val t = T::class.java.getMethod(\"top\").invoke(null)
    if (t != \"4t\") return \"top: $t\"
    return \"OK\"
}
";

#[test]
fn overloads_fill_omitted_defaults_at_run_time() {
    common::expect_box_same_as_kotlinc(CALLED, "jvm_overloads_called");
}
