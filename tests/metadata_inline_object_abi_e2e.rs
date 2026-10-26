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
