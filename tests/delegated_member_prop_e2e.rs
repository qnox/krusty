//! Member delegated properties `class A { val/var x by Del() }`: an instance `x$delegate` field
//! (initialized in `<init>`) + a static `x$kprop` (`PropertyReference1Impl`) + an instance `getX()`
//! calling `this.x$delegate.getValue(this, x$kprop)` (and `setX` via `setValue` for `var`). The
//! delegate's `getValue`/`setValue` here ignore the property argument. Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn member_delegated_val_runs() {
    // Exact shape of corpus inClassVal.kt.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    operator fun getValue(t: Any?, p: KProperty<*>): Int = 1\n\
}\n\
class A {\n\
    val prop: Int by Delegate()\n\
}\n\
fun box(): String = if (A().prop == 1) \"OK\" else \"fail\"\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn member_delegated_var_runs() {
    // Exact shape of corpus inClassVar.kt.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    var inner = 1\n\
    operator fun getValue(t: Any?, p: KProperty<*>): Int = inner\n\
    operator fun setValue(t: Any?, p: KProperty<*>, i: Int) { inner = i }\n\
}\n\
class A {\n\
    var prop: Int by Delegate()\n\
}\n\
fun box(): String {\n\
    val c = A()\n\
    if (c.prop != 1) return \"fail get\"\n\
    c.prop = 2\n\
    if (c.prop != 2) return \"fail set\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn inferred_anonymous_override_delegate_uses_its_pass_two_type() {
    // Regression for KT-76171's essential shape. `expectedValue` is an ordinary declaration inside
    // an anonymous class, so its inferred `R` is discovered while that Pass-2 unit is checked. The
    // temporary Pass-1 header is not a semantic type and must not reach common IR or JVM bridges.
    const SRC: &str = r#"
        import kotlin.reflect.KProperty

        interface DialogScope<R> {
            var expectedValue: R
        }

        fun <T> rememberA(calculation: () -> T): T = calculation()

        class FakeMutableState<T>(var value: T) {
            operator fun getValue(thisRef: Any?, property: KProperty<*>): T = value
            operator fun setValue(thisRef: Any?, property: KProperty<*>, newValue: T) {
                value = newValue
            }
        }

        fun <T> fakeMutableStateOf(value: T): FakeMutableState<T> = FakeMutableState(value)

        class DialogState {
            fun <R> awaitResult(initial: R): R {
                val state = rememberA { fakeMutableStateOf(initial) }
                rememberA {
                    object : DialogScope<R> {
                        override var expectedValue by state
                    }
                }.expectedValue = "OK" as R
                return state.value
            }
        }

        fun box(): String = DialogState().awaitResult("fail")
    "#;

    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn inherited_classpath_member_extension_is_a_stable_delegate_target() {
    const LIB: &str = r#"
        package api

        import kotlin.reflect.KProperty

        open class Base {
            operator fun Int.getValue(owner: Any?, property: KProperty<*>): String = property.name
        }
    "#;
    const MAIN: &str = r#"
        import api.Base
        import kotlin.reflect.KProperty

        open class Middle : Base()

        class Owner : Middle() {
            val result by 1
        }

        fun box(): String = if (Owner().result == "result") "OK" else "fail"
    "#;

    let Some(output) = common::expect_box_run_against_kotlinc(LIB, MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}
