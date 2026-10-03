//! A value class's suspend interface override is the CPS entry on the box.
//!
//! The member itself is the static `build-impl(carrier, Continuation)Object`. The box implements
//! the interface with `build(Continuation)Object`, which reads the carrier and forwards the
//! continuation. The declared `build()String` entry calls a method the static replacement no longer
//! has.

use super::common;

const SOURCE: &str = "\
import kotlin.coroutines.*\n\
\n\
fun builder(c: suspend () -> Unit) {\n\
    c.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
}\n\
\n\
class Delegate {\n\
    fun build(): String = \"OK\"\n\
}\n\
\n\
interface Digest {\n\
    suspend fun build(): String\n\
}\n\
\n\
@JvmInline\n\
value class DigestImpl(val delegate: Delegate) : Digest {\n\
    override suspend fun build(): String = delegate.build()\n\
}\n\
\n\
fun box(): String {\n\
    var res = \"FAIL\"\n\
    val digest: Digest = DigestImpl(Delegate())\n\
    builder {\n\
        res = digest.build()\n\
    }\n\
    return res\n\
}\n";

#[test]
fn a_value_class_suspend_override_delegates_through_the_cps_entry() {
    common::expect_box_ok_with_stdlib(SOURCE, "ValueClassSuspendOverride");
    let classes = common::expect_classes_with_stdlib(SOURCE, "ValueClassSuspendOverride");
    let digest = classes
        .iter()
        .find(|(name, _)| name == "DigestImpl")
        .expect("DigestImpl is emitted");
    let dir = common::scratch_dir().expect("a scratch directory");
    let class_file = dir.join("DigestImpl.class");
    std::fs::write(&class_file, &digest.1).expect("DigestImpl class file");
    let text = common::javap(&["-p", "-c", &class_file.to_string_lossy()])
        .expect("javap disassembles DigestImpl");
    assert!(
        text.contains(
            "static java.lang.Object build-impl(Delegate, kotlin.coroutines.Continuation"
        ),
        "{text}"
    );
    assert!(
        text.contains("java.lang.Object build(kotlin.coroutines.Continuation"),
        "{text}"
    );
    assert!(
        text.lines().any(|line| {
            line.contains("invokestatic")
                && line.contains(
                    "build-impl:(LDelegate;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
                )
        }),
        "{text}"
    );
    assert!(
        !text.contains("java.lang.String build()"),
        "the declared signature is not the interface entry: {text}"
    );
}
