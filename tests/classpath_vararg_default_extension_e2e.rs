//! A dependency extension whose `vararg` declares a default must evaluate that default when the
//! call omits it. The flag lives on the provider call signature (`declares_default_value`); the
//! dependency has no local declaration for checked FIR to query. Supplying elements still packs
//! them and does not run the default again.

use super::common;

const LIB: &str = r#"
package dep

class Payload

var calls = 0

fun note(): IntArray {
    calls += 1
    return intArrayOf(1, 2)
}

fun Payload.pack(vararg xs: Int = note()): Int {
    var sum = 0
    for (value in xs) sum += value
    return sum * 10 + calls
}

class Host {
    var defaults = 0
    fun Payload.bundle(vararg xs: Int = run { defaults += 1; intArrayOf(1, 2) }): Int {
        var sum = 0
        for (value in xs) sum += value
        return sum * 10 + defaults
    }
}
"#;

#[test]
fn classpath_extension_omitted_vararg_default_executes() {
    common::Fixture::new()
        .reference_lib("Lib.kt", LIB)
        .assert_box_ok(
            r#"
        import dep.Payload
        import dep.pack
        fun box(): String {
            val omitted = Payload().pack()
            val given = Payload().pack(42)
            return if (omitted == 31 && given == 421) "OK" else "$omitted/$given"
        }
        "#,
        );
}

#[test]
fn classpath_member_extension_omitted_vararg_default_executes() {
    common::Fixture::new()
        .reference_lib("Lib.kt", LIB)
        .assert_box_ok(
            r#"
        import dep.Host
        import dep.Payload
        fun box(): String {
            val host = Host()
            val omitted = with(host) { Payload().bundle() }
            val given = with(host) { Payload().bundle(40, 2) }
            return if (omitted == 31 && given == 421) "OK" else "$omitted/$given"
        }
        "#,
        );
}
