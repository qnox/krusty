//! Top-level and member suspend functions whose state machine kotlinc's coroutine transformer
//! builds from the finished method (`docs/JVM_INLINE_BEFORE_CPS.md` §1a, steps 2 and 3). An
//! overridable member's machine is built in the static `$suspendImpl` its body moves to.
//!
//! The emitter marks each suspension point as kotlinc's codegen does and the transformer rewrites
//! the method when its class is written. The continuation class it describes (spill fields, then
//! `result` and `label`, and the `@DebugMetadata` arrays) is byte-identical to kotlinc's, and so
//! are the function's own instructions once kotlinc's optimizer has run over the transformed body.

use super::common;

const WORK: &str = "suspend fun leaf() {}\n\
suspend fun value(): Int = 1\n\
suspend fun work(count: Int, ref: String): Int {\n\
    leaf()\n\
    val v = value()\n\
    return v + count + ref.length\n\
}\n";

/// `class` compiled by both compilers, against the standard library. Without it krusty's runtime
/// capabilities do not include the spill clean-up the transformer needs, and the IR machine builds
/// the function instead.
fn transformed_class_diff(name: &str, src: &str, class: &str) -> Option<Result<(), String>> {
    common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()])
}

#[test]
fn a_continuation_spilling_an_int_and_a_reference_matches_kotlinc() {
    let result = transformed_class_diff("TransformerWork", WORK, "TransformerWorkKt$work$1")
        .expect("reference kotlinc is provisioned");
    result.expect("the continuation class is byte-identical to kotlinc's");
}

#[test]
fn the_transformed_function_s_instructions_match_kotlinc() {
    // The optimizer runs after the transform, as kotlinc chains `OptimizationMethodVisitor` after
    // `CoroutineTransformerMethodVisitor`: it drops the transformer's leftover temporaries and
    // compacts the spill slots.
    match common::method_code_diff_against_kotlinc(
        "TransformerWorkCode",
        &[],
        WORK,
        "TransformerWorkCodeKt",
        "public static final java.lang.Object work(",
    ) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

#[test]
fn a_unit_function_continuation_matches_kotlinc() {
    // The last suspension's next line is the closing `}`, where kotlinc maps the implicit return.
    let src = "suspend fun leaf() {}\n\
suspend fun work(count: Int, ref: String) {\n\
    leaf()\n\
    leaf()\n\
}\n";
    let result = transformed_class_diff("TransformerUnit", src, "TransformerUnitKt$work$1")
        .expect("reference kotlinc is provisioned");
    result.expect("the continuation class is byte-identical to kotlinc's");
}

#[test]
fn a_backticked_hyphen_stays_in_the_continuation_source_name() {
    let src = "suspend fun leaf() {}\n\
suspend fun `work-item`(): Int {\n\
    leaf()\n\
    return 1\n\
}\n";
    let result = common::byte_diff_against_kotlinc(
        "TransformerBacktick",
        src,
        "TransformerBacktickKt$work-item$1",
    )
    .expect("reference kotlinc is provisioned");
    result.expect("the continuation keeps the exact source identifier like kotlinc's");
}

#[test]
fn spilled_locals_survive_real_suspensions() {
    // `pause` really suspends; `work` resumes twice from a continuation parked outside it, so its
    // spilled `count` and `ref` are restored from the continuation's fields.
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Unit>? = null\n\
suspend fun pause() = suspendCoroutine<Unit> { parked = it }\n\
suspend fun work(count: Int, ref: String): String {\n\
    pause()\n\
    val first = ref + count\n\
    pause()\n\
    return first + ref.length\n\
}\n\
fun box(): String {\n\
    var r = \"none\"\n\
    suspend { r = work(3, \"ab\") }.startCoroutine(Done())\n\
    parked!!.resume(Unit)\n\
    parked!!.resume(Unit)\n\
    return if (r == \"ab32\") \"OK\" else \"F:$r\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "TransformerResume");
}

#[test]
fn a_suspend_lambda_keeps_its_own_continuation_beside_the_enclosing_function_s_classes() {
    // The lambda records `box` as its enclosing source name. Its continuation keeps the name it
    // was lifted under, so it does not take the `box$N` of a callable reference's class.
    let src = "import kotlin.coroutines.*\n\
suspend fun value(): String = \"O\"\n\
suspend fun k(): String = \"K\"\n\
fun run(c: suspend () -> String): String {\n\
    var r = \"none\"\n\
    c.startCoroutine(object : Continuation<String> {\n\
        override val context: CoroutineContext = EmptyCoroutineContext\n\
        override fun resumeWith(result: Result<String>) { r = result.getOrThrow() }\n\
    })\n\
    return r\n\
}\n\
fun box(): String {\n\
    val first = run {\n\
        value()\n\
        value()\n\
    }\n\
    return first + run(::k) + run(::k).drop(1)\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "TransformerLambdaNames");
}

const MEMBERS: &str = "suspend fun leaf() {}\n\
suspend fun value(): Int = 1\n\
class Box(val base: Int) {\n\
    suspend fun work(count: Int, ref: String): Int {\n\
        leaf()\n\
        val v = value()\n\
        return v + count + ref.length + base\n\
    }\n\
}\n\
object Single {\n\
    suspend fun work(count: Int): Int {\n\
        leaf()\n\
        return count + value()\n\
    }\n\
}\n";

#[test]
fn a_final_member_s_continuation_captures_its_receiver_like_kotlinc() {
    // The continuation nests under the class and takes the receiver as `this$0`, which
    // `invokeSuspend` re-enters the member through.
    for class in ["Box$work$1", "Single$work$1"] {
        let result = transformed_class_diff("TransformerMembers", MEMBERS, class)
            .expect("reference kotlinc is provisioned");
        result.expect("the continuation class is byte-identical to kotlinc's");
    }
}

#[test]
fn a_final_member_s_instructions_match_kotlinc() {
    for (class, method) in [
        ("Box", "public final java.lang.Object work("),
        ("Single", "public final java.lang.Object work("),
    ] {
        match common::method_code_diff_against_kotlinc(
            "TransformerMemberCode",
            &[],
            MEMBERS,
            class,
            method,
        ) {
            None => panic!("reference kotlinc is provisioned"),
            Some(Ok(())) => {}
            Some(Err(difference)) => panic!("{difference}"),
        }
    }
}

const OPEN_MEMBERS: &str = "suspend fun leaf() {}\n\
suspend fun value(): Int = 1\n\
suspend fun wide(): Long = 2L\n\
open class Base(val base: Int) {\n\
    open suspend fun work(count: Int, ref: String): Int {\n\
        leaf()\n\
        val v = value()\n\
        return v + count + ref.length + base\n\
    }\n\
}\n\
sealed class Shape {\n\
    open suspend fun area(scale: Long): Long {\n\
        leaf()\n\
        return scale * wide()\n\
    }\n\
}\n\
abstract class Store {\n\
    open suspend fun fetch(key: String, retries: Int = 3): String {\n\
        leaf()\n\
        return key + retries\n\
    }\n\
}\n";

#[test]
fn an_overridable_member_s_continuation_re_enters_its_suspend_impl_like_kotlinc() {
    // The continuation still captures the receiver as `this$0`, but `invokeSuspend` passes it to
    // the static `$suspendImpl`, which is also its enclosing method and `@DebugMetadata`'s `m`.
    for class in ["Base$work$1", "Shape$area$1", "Store$fetch$1"] {
        let result = transformed_class_diff("TransformerOpenMembers", OPEN_MEMBERS, class)
            .expect("reference kotlinc is provisioned");
        result.expect("the continuation class is byte-identical to kotlinc's");
    }
}

#[test]
fn an_overridable_member_forwards_to_the_suspend_impl_that_carries_its_machine() {
    // The member is a trampoline without a line table; the package-private synthetic static takes
    // the receiver as `$this` and holds the machine, which spills that receiver like a parameter.
    for (class, method) in [
        ("Base", "public java.lang.Object work("),
        ("Base", "static java.lang.Object work$suspendImpl("),
        ("Shape", "public java.lang.Object area("),
        ("Shape", "static java.lang.Object area$suspendImpl("),
        ("Store", "public java.lang.Object fetch("),
        ("Store", "static java.lang.Object fetch$suspendImpl("),
        ("Store", "public static java.lang.Object fetch$default("),
    ] {
        match common::method_code_diff_against_kotlinc(
            "TransformerOpenMemberCode",
            &[],
            OPEN_MEMBERS,
            class,
            method,
        ) {
            None => panic!("reference kotlinc is provisioned"),
            Some(Ok(())) => {}
            Some(Err(difference)) => panic!("{difference}"),
        }
    }
}

const OVERRIDE_LIB: &str = "package lib\n\
suspend fun leaf() {}\n\
suspend fun value(): Int = 1\n";

const OVERRIDE_MEMBERS: &str = "import lib.*\n\
open class Root {\n\
    open suspend fun work(count: Int): Int {\n\
        leaf()\n\
        return count + value()\n\
    }\n\
    open suspend fun done(count: Int): Int {\n\
        leaf()\n\
        return count + value()\n\
    }\n\
}\n\
open class Mid : Root() {\n\
    override suspend fun work(count: Int): Int {\n\
        leaf()\n\
        return count + value() + 1\n\
    }\n\
    final override suspend fun done(count: Int): Int {\n\
        leaf()\n\
        return count + value() + 2\n\
    }\n\
    suspend fun plain(count: Int): Int {\n\
        leaf()\n\
        return count + value() + 3\n\
    }\n\
}\n";

#[test]
fn only_an_implicitly_open_override_in_an_open_class_splits() {
    // `Mid.work` overrides without `final`, so it stays open: a trampoline beside the
    // `work$suspendImpl` that holds its machine. `Mid.done` is a `final override` and `Mid.plain`
    // is implicitly final, so each keeps its machine and no static is written beside it.
    let classes = common::classes_against_kotlinc_lib(
        "TransformerOverrideMembers",
        &[("Lib.kt", OVERRIDE_LIB)],
        OVERRIDE_MEMBERS,
    )
    .expect("reference kotlinc is provisioned");
    let (reference, krusty) = classes
        .method_declarations("Mid")
        .expect("both compilers write Mid");
    assert!(
        reference
            .iter()
            .any(|method| method.contains("work$suspendImpl("))
            && !reference
                .iter()
                .any(|method| method.contains("done$suspendImpl(")
                    || method.contains("plain$suspendImpl(")),
        "kotlinc splits only the open override: {reference:#?}"
    );
    assert_eq!(
        krusty, reference,
        "Mid declares kotlinc's methods, in its order"
    );
    for class in ["Mid$work$1", "Mid$done$1", "Mid$plain$1"] {
        assert_eq!(
            classes.krusty.get(class),
            classes.reference.get(class),
            "{class} is byte-identical to kotlinc's"
        );
    }
}

#[test]
fn a_final_member_of_an_open_class_keeps_its_machine_like_kotlinc() {
    for method in [
        "public java.lang.Object work(",
        "static java.lang.Object work$suspendImpl(",
        "public final java.lang.Object done(",
        "public final java.lang.Object plain(",
    ] {
        match common::method_code_diff_against_kotlinc(
            "TransformerOverrideMemberCode",
            &[("Lib.kt", OVERRIDE_LIB)],
            OVERRIDE_MEMBERS,
            "Mid",
            method,
        ) {
            None => panic!("reference kotlinc is provisioned"),
            Some(Ok(())) => {}
            Some(Err(difference)) => panic!("{difference}"),
        }
    }
}

#[test]
fn an_overridable_member_resumes_in_its_own_body_under_an_override_elsewhere() {
    // A control, not the re-entry hazard: `Plain` inherits `Base.work` and suspends in it, and
    // `Other`'s override is dispatched to through the same member. The hazard itself, an override
    // whose `super` call suspends in the base body, is
    // `a_super_call_resumes_in_the_base_body_rather_than_the_override`.
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Unit>? = null\n\
suspend fun pause() = suspendCoroutine<Unit> { parked = it }\n\
open class Base(val tag: String) {\n\
    open suspend fun work(count: Int): String {\n\
        pause()\n\
        val first = tag + count\n\
        pause()\n\
        return first + tag\n\
    }\n\
}\n\
class Plain : Base(\"p\")\n\
class Other : Base(\"o\") {\n\
    override suspend fun work(count: Int): String = \"other\" + count\n\
}\n\
fun box(): String {\n\
    var r = \"none\"\n\
    var s = \"none\"\n\
    val plain: Base = Plain()\n\
    val other: Base = Other()\n\
    suspend { r = plain.work(3) }.startCoroutine(Done())\n\
    parked!!.resume(Unit)\n\
    parked!!.resume(Unit)\n\
    suspend { s = other.work(4) }.startCoroutine(Done())\n\
    return if (r == \"p3p\" && s == \"other4\") \"OK\" else \"F:$r:$s\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "TransformerOpenResume");
}

#[test]
fn a_super_call_resumes_in_the_base_body_rather_than_the_override() {
    // `Wrapped.work` calls `super.work`, which suspends inside `Base`'s body. Resuming must
    // re-enter that body through the static `$suspendImpl`; re-entering through the virtual
    // member would dispatch back to the override and run it a second time.
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Unit>? = null\n\
var entries = 0\n\
suspend fun pause() = suspendCoroutine<Unit> { parked = it }\n\
open class Base(val tag: String) {\n\
    open suspend fun work(count: Int): String {\n\
        pause()\n\
        val first = tag + count\n\
        pause()\n\
        return first + tag\n\
    }\n\
}\n\
class Wrapped : Base(\"w\") {\n\
    override suspend fun work(count: Int): String {\n\
        entries++\n\
        return \"[\" + super.work(count) + \"]\"\n\
    }\n\
}\n\
fun box(): String {\n\
    var r = \"none\"\n\
    val wrapped: Base = Wrapped()\n\
    suspend { r = wrapped.work(3) }.startCoroutine(Done())\n\
    parked!!.resume(Unit)\n\
    parked!!.resume(Unit)\n\
    return if (r == \"[w3w]\" && entries == 1) \"OK\" else \"F:$r:$entries\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "TransformerSuperResume");
}
