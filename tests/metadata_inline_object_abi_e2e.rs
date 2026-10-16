//! `@Metadata.xi` of a class declared in an `inline` function's body, measured against kotlinc
//! 2.4.20. Code that inlines the function copies the class, so kotlinc marks it public ABI
//! (`METADATA_PUBLIC_ABI_FLAG`) at any depth below an `inline` function that is neither private nor
//! a member of a private class. A class outside every such function is not marked.

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
            "app/InlineObjectsKt$plain$1",
        ],
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{diff}"));
}
