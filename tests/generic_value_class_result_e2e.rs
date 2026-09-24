//! A generic function whose declared result is a value class instantiated with the function's own
//! type parameters (`fun <A> of(v: A): Tagged<A>`). The type arguments have no representation: the
//! declaration returns the value class's carrier, exactly as a non-generic one does, so the call
//! site reads the carrier directly. Only a bare type parameter (`fun <T> id(v: T): T`) crosses an
//! erased slot and hands back a box. Treating the first like the second unboxed a carrier that was
//! never boxed, and the JVM rejected the caller with a VerifyError.
use super::common;

const TAGS: &str = "\
@JvmInline
value class Tagged<out A>(val label: String)

@JvmInline
value class Either<out L, out R> internal constructor(val slot: Any?)

object Tags {
    fun <A> of(value: A): Tagged<A> = Tagged(value.toString())
    fun <L> left(value: L): Either<L, Nothing> = Either(value)
}

fun box(): String {
    val tagged: Tagged<Int> = Tags.of(7)
    if (tagged.label != \"7\") return \"FAIL 1\"
    val sided: Either<Int, String> = Tags.left(3)
    if (sided.slot != 3) return \"FAIL 2\"
    val inferred = Tags.of(\"OK\")
    return inferred.label
}
";

#[test]
fn generic_value_class_result_is_read_as_its_carrier() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let out = common::compile_and_run_box(
        TAGS,
        "GenericValueClassResult",
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(out.as_deref(), Some("OK"));
}

#[test]
fn generic_value_class_result_call_site_matches_kotlinc() {
    match common::byte_diff_against_kotlinc_cp(
        "GenericValueClassResult",
        TAGS,
        "GenericValueClassResultKt",
        &[common::stdlib_jar()],
    ) {
        None => eprintln!("skip (GenericValueClassResult: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(why)) => panic!("{why}"),
    }
}

/// The shape reduced from kotlin-result's `Ok`/`Err` factories: top-level generic factories over a
/// two-parameter value class with a nullable `Any?` carrier.
#[test]
fn generic_value_class_factories_run() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    const SOURCE: &str = "\
@JvmInline
value class Wrap<out A, out B> internal constructor(private val v: Any?) {
    val raw: Any? get() = v
}

fun <A> Left(value: A): Wrap<A, Nothing> = Wrap(value)
fun <B> Right(value: B): Wrap<Nothing, B> = Wrap(value)

fun box(): String {
    val a: Wrap<Int, String> = Left(7)
    val b: Wrap<Int, String> = Right(\"boom\")
    return if (a.raw == 7 && b.raw == \"boom\") \"OK\" else \"FAIL\"
}
";
    let out = common::compile_and_run_box(
        SOURCE,
        "GenericValueClassFactories",
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(out.as_deref(), Some("OK"));
}
