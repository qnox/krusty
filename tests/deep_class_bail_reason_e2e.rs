//! Production checked-FIR regressions for class shapes once rejected only by the retired AST lowerer.
//! Each fixture exercises the formerly gated behavior at runtime; merely observing emitted bytes
//! would allow a verifier or dispatch failure to masquerade as support.
//!
use super::common;

#[test]
fn suspend_covariant_value_class_override_runs_through_interface() {
    common::expect_box_ok_with_stdlib(
        r#"
import kotlin.coroutines.*

@JvmInline
value class IC(val s: String)

interface IBar {
    suspend fun bar(): Any
}

class Test : IBar {
    override suspend fun bar(): IC = IC("OK")
}

fun box(): String {
    var result = "fail"
    val invoke: suspend () -> Any = { (Test() as IBar).bar() }
    invoke.startCoroutine(Continuation(EmptyCoroutineContext) {
        result = (it.getOrThrow() as IC).s
    })
    return result
}
"#,
        "SuspendCovariantValueClassOverride",
    );
}

#[test]
fn suspending_lambda_in_class_member_captures_dispatch_receiver() {
    common::expect_box_ok_with_stdlib(
        r#"
import kotlin.coroutines.*

class C {
    private val value = 1
    suspend fun m(): Int = value
    fun g(): suspend () -> Int = suspend { m() }
}

fun box(): String {
    var result = 0
    C().g().startCoroutine(Continuation(EmptyCoroutineContext) {
        result = it.getOrThrow()
    })
    return if (result == 1) "OK" else "fail: $result"
}
"#,
        "SuspendingLambdaClassReceiver",
    );
}

#[test]
fn enum_external_supertype_obligation_dispatches() {
    common::expect_box_ok_with_stdlib(
        r#"
enum class ExternalEnum : java.util.function.Supplier<String> {
    ONLY;
    override fun get(): String = "OK"
}

fun box(): String = ExternalEnum.ONLY.get()
"#,
        "EnumExternalSupertype",
    );
}

#[test]
fn generic_enum_entry_override_dispatches_through_interface() {
    common::expect_box_ok_with_stdlib(
        r#"
interface GenericAction<T> {
    fun apply(value: T): String
}

enum class EntryEnum : GenericAction<String> {
    ONLY {
        override fun apply(value: String): String = value
    }
}

fun box(): String {
    val action: GenericAction<String> = EntryEnum.ONLY
    return action.apply("OK")
}
"#,
        "GenericEnumEntryOverride",
    );
}

#[test]
fn enum_entry_custom_property_getter_runs() {
    common::expect_box_ok_with_stdlib(
        r#"
enum class PropertyEnum {
    ONLY {
        override val value: String get() = "OK"
    };

    abstract val value: String
}

fun box(): String = PropertyEnum.ONLY.value
"#,
        "EnumEntryCustomProperty",
    );
}

#[test]
fn user_class_name_cannot_impersonate_an_anonymous_object() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    // This ordinary declaration intentionally resembles the parser's current synthetic-name format.
    // Anonymous-object policy must follow the AST ownership map, never generated or user-written text;
    // the old substring check incorrectly applied the outer-capture gate to this valid class.
    const SRC: &str = r#"
open class Base(val value: Int)

class `Regular$anon$Name` : Base(1) {
    fun result(): Int = value
}

fun box(): String = if (`Regular$anon$Name`().result() == 1) "OK" else "fail"
"#;
    assert_eq!(
        common::inline_source_backend_outcome(SRC),
        Some(common::BackendOutcome::Emitted),
        "a user class name must not activate anonymous-object lowering policy"
    );
}
