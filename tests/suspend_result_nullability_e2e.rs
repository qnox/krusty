//! A suspend function returns its result or `COROUTINE_SUSPENDED` through the CPS `Object`, which
//! kotlinc annotates `@Nullable` whatever the declared result: a value class, a bare type
//! parameter, or an abstract declaration's.

use super::common;

const SOURCE: &str = r#"@JvmInline
value class Box(val x: Int)

@JvmInline
value class Ref(val s: String)

interface Api {
    suspend fun named(): String

    suspend fun <T> generic(): T
}

abstract class Base {
    abstract suspend fun boxed(): Box
}

class Impl : Base(), Api {
    override suspend fun named(): String = "a"

    @Suppress("UNCHECKED_CAST")
    override suspend fun <T> generic(): T = null as T

    override suspend fun boxed(): Box = Box(1)

    suspend fun reference(): Ref = Ref("r")

    suspend fun nullableReference(): Ref? = null

    suspend fun scalar(): Int = 1

    suspend fun <T : Any> bounded(value: T): T = value
}

suspend fun topBoxed(): Box = Box(2)

suspend fun <T> topGeneric(value: T): T = value
"#;

#[test]
fn a_suspend_result_is_annotated_nullable_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendResultNullability",
        SOURCE,
        &["Api", "Base", "Impl", "SuspendResultNullabilityKt"],
    );
}
