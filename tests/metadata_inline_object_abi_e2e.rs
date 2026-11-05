//! `@Metadata.xi` of a class declared in an `inline` function's body, measured against kotlinc
//! 2.4.20. Code that inlines the function copies the class, so kotlinc marks it public ABI
//! (`METADATA_PUBLIC_ABI_FLAG`) at any depth below an `inline` function that is neither private nor
//! a member of a private class; only the function's own class counts, not one it is nested in. A
//! class outside every such function is not marked.

use super::common;

const SOURCE: &str = r#"package app

interface Greeter {
    fun greet(): String
}

inline fun public(): Greeter = object : Greeter {
    override fun greet() = "public"
}

internal inline fun internal(): Greeter = object : Greeter {
    override fun greet() = "internal"
}

@PublishedApi
internal inline fun published(): Greeter = object : Greeter {
    override fun greet() = "published"
}

private inline fun hidden(): Greeter = object : Greeter {
    override fun greet() = "hidden"
}

fun useHidden(): Greeter = hidden()

inline fun nested(): Greeter = object : Greeter {
    override fun greet() = object : Greeter {
        override fun greet() = "nested"
    }.greet()
}

inline fun inLambda(): Greeter = run {
    listOf(1).map { object : Greeter { override fun greet() = "lambda" } }.first()
}

class Open {
    inline fun member(): Greeter = object : Greeter {
        override fun greet() = "member"
    }
}

private class Closed {
    inline fun member(): Greeter = object : Greeter {
        override fun greet() = "closed"
    }
}

private class Outer {
    class Nested {
        inline fun member(): Greeter = object : Greeter {
            override fun greet() = "nested in private"
        }
    }
}

class Shell {
    private class Inner {
        inline fun member(): Greeter = object : Greeter {
            override fun greet() = "private nested"
        }
    }
}

fun plain(): Greeter = object : Greeter {
    override fun greet() = "plain"
}
"#;

#[test]
fn a_class_in_an_inline_body_is_marked_public_abi_like_kotlinc() {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_headers_diff_against_kotlinc_cp(
        "InlineObjects",
        SOURCE,
        &[
            "app/InlineObjectsKt$public$1",
            "app/InlineObjectsKt$internal$1",
            "app/InlineObjectsKt$published$1",
            "app/InlineObjectsKt$hidden$1",
            "app/InlineObjectsKt$nested$1",
            "app/InlineObjectsKt$nested$1$greet$1",
            "app/InlineObjectsKt$inLambda$1$1$1",
            "app/Open$member$1",
            "app/Closed$member$1",
            "app/Outer$Nested$member$1",
            "app/Shell$Inner$member$1",
            "app/InlineObjectsKt$plain$1",
        ],
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{diff}"));
}

/// The classes kotlinc synthesizes for a reference or an annotation construction follow the same
/// rule. A reference class is marked when it is written in a public inline function. An annotation
/// implementation is one class per annotation and file, and kotlinc decides when it creates the
/// class, at the first construction in declaration order: `A` is constructed by `plain` before
/// `public`, so it stays unmarked, while `B`, constructed only in a lambda of an inline function,
/// and `D`, constructed in an inline member, are marked.
const SYNTHESIZED_SOURCE: &str = r#"package app

annotation class A(val i: Int)
annotation class B(val i: Int)
annotation class D(val i: Int)

fun plain(i: Int): Any = A(i)
inline fun public(i: Int): Any = A(i)
inline fun inLambda(i: Int): () -> Any = { B(i) }

class K {
    inline fun member(i: Int): Any = D(i)
}

class P(val x: Int)
val top = 1
fun one() = 1

inline fun propertyRef(): Any = P::x
inline fun topRef(): Any = ::top
inline fun functionRef(): Any = ::one
fun plainRef(): Any = P::x
"#;

#[test]
fn a_synthesized_reference_or_annotation_class_is_marked_public_abi_like_kotlinc() {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_headers_diff_against_kotlinc_cp(
        "Synthesized",
        SYNTHESIZED_SOURCE,
        &[
            "app/SynthesizedKt$annotationImpl$app_A$0",
            "app/SynthesizedKt$annotationImpl$app_B$0",
            "app/K$annotationImpl$app_D$0",
            "app/SynthesizedKt$inLambda$1",
            "app/SynthesizedKt$propertyRef$1",
            "app/SynthesizedKt$topRef$1",
            "app/SynthesizedKt$functionRef$1",
            "app/SynthesizedKt$plainRef$1",
        ],
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{diff}"));
}

/// A top-level property's initializer is a declaration of its own, ordered against the functions
/// around it by source position: `A` is first constructed by `first`'s initializer, before the
/// inline function, so its implementation stays unmarked; `B` is first constructed in the inline
/// function, before `second`'s initializer, so it is marked.
const PROPERTY_ORDER_SOURCE: &str = r#"package app

annotation class A(val i: Int)
annotation class B(val i: Int)

val first: Any = A(0)
inline fun later(): Any = A(1)

inline fun earlier(): Any = B(1)
val second: Any = B(0)
"#;

#[test]
fn a_property_initializer_takes_its_declaration_order_against_an_inline_function() {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_headers_diff_against_kotlinc_cp(
        "PropertyOrder",
        PROPERTY_ORDER_SOURCE,
        &[
            "app/PropertyOrderKt$annotationImpl$app_A$0",
            "app/PropertyOrderKt$annotationImpl$app_B$0",
        ],
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{diff}"));
}

/// The same for a class's property initializers and `init` blocks against its inline members.
const CLASS_INITIALIZER_ORDER_SOURCE: &str = r#"package app

annotation class A(val i: Int)
annotation class B(val i: Int)
annotation class C(val i: Int)
annotation class D(val i: Int)

class K {
    val a: Any = A(0)
    inline fun m(): Any = A(1)
    inline fun n(): Any = B(1)
    val b: Any = B(0)
    init { C(0) }
    inline fun o(): Any = C(1)
    inline fun p(): Any = D(1)
    init { D(0) }
}
"#;

#[test]
fn a_class_initializer_takes_its_declaration_order_against_an_inline_member() {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_headers_diff_against_kotlinc_cp(
        "ClassInitializerOrder",
        CLASS_INITIALIZER_ORDER_SOURCE,
        &[
            "app/K$annotationImpl$app_A$0",
            "app/K$annotationImpl$app_B$0",
            "app/K$annotationImpl$app_C$0",
            "app/K$annotationImpl$app_D$0",
        ],
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{diff}"));
}

/// A secondary constructor is a declaration of its own, its delegation's arguments included:
/// `A` and `C` are first constructed by a constructor before the inline member (in the delegation's
/// arguments and in the body), so their implementations stay unmarked; `B` and `D` are first
/// constructed by an inline member before the constructor, so they are marked.
const CONSTRUCTOR_ORDER_SOURCE: &str = r#"package app

annotation class A(val i: Int)
annotation class B(val i: Int)
annotation class C(val i: Int)
annotation class D(val i: Int)

open class Base(val any: Any)

class K : Base {
    constructor() : super(A(0))
    inline fun later(): Any = A(1)
    inline fun earlier(): Any = B(1)
    constructor(i: Int) : super(B(0))
    constructor(s: String) : super(s) { C(0) }
    inline fun lastly(): Any = C(1)
    inline fun before(): Any = D(1)
    constructor(l: Long) : super(l) { D(0) }
}
"#;

#[test]
fn a_secondary_constructor_takes_its_declaration_order_against_an_inline_member() {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_headers_diff_against_kotlinc_cp(
        "ConstructorOrder",
        CONSTRUCTOR_ORDER_SOURCE,
        &[
            "app/K$annotationImpl$app_A$0",
            "app/K$annotationImpl$app_B$0",
            "app/K$annotationImpl$app_C$0",
            "app/K$annotationImpl$app_D$0",
        ],
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{diff}"));
}

/// A default argument is evaluated by its declaration's `$default` wrapper, so it takes that
/// function's or constructor's order: `A` (primary constructor), `B` (secondary constructor), `D`
/// (member) and `F` (top-level function) are first constructed by a default before the inline
/// function, so their implementations stay unmarked; `C`, `E` and `G` are first constructed by an
/// inline function before the declaration whose default constructs them, so they are marked. `H`
/// is a public inline function's own default, ahead of a non-inline construction.
const DEFAULT_ORDER_SOURCE: &str = r#"package app

annotation class A(val i: Int)
annotation class B(val i: Int)
annotation class C(val i: Int)
annotation class D(val i: Int)
annotation class E(val i: Int)
annotation class F(val i: Int)
annotation class G(val i: Int)
annotation class H(val i: Int)

class K(val p: Any = A(0)) {
    constructor(x: Any = B(0), y: Int) : this(x)
    inline fun later(): Any = A(1)
    inline fun before(): Any = C(1)
    constructor(x: Any = C(0), y: Long) : this(x)
    inline fun mid(): Any = B(1)
    fun member(x: Any = D(0)): Any = x
    inline fun d(): Any = D(1)
    inline fun e(): Any = E(1)
    fun memberAfter(x: Any = E(0)): Any = x
}

fun top(x: Any = F(0)): Any = x
inline fun f(): Any = F(1)
inline fun g(): Any = G(1)
fun topAfter(x: Any = G(0)): Any = x
inline fun h(x: Any = H(0)): Any = x
fun notInline(): Any = H(1)
"#;

#[test]
fn a_default_argument_takes_its_declaration_order_against_an_inline_function() {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::metadata_headers_diff_against_kotlinc_cp(
        "DefaultOrder",
        DEFAULT_ORDER_SOURCE,
        &[
            "app/K$annotationImpl$app_A$0",
            "app/K$annotationImpl$app_B$0",
            "app/K$annotationImpl$app_C$0",
            "app/K$annotationImpl$app_D$0",
            "app/K$annotationImpl$app_E$0",
            "app/DefaultOrderKt$annotationImpl$app_F$0",
            "app/DefaultOrderKt$annotationImpl$app_G$0",
            "app/DefaultOrderKt$annotationImpl$app_H$0",
        ],
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{diff}"));
}
