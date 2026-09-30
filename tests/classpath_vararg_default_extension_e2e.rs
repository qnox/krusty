//! A dependency extension whose `vararg` declares a default must evaluate that default when the
//! call omits it. The flag lives on the provider call signature (`declares_default_value`); the
//! dependency has no local declaration for checked FIR to query. Supplying elements still packs
//! them and does not run the default again.

use super::common;

const LIB: &str = r#"
package dep

var log = ""

fun note(): IntArray {
    log += "d"
    return intArrayOf(1, 2)
}

fun String.pack(vararg xs: Int = note()): String {
    var sum = 0
    for (value in xs) sum += value
    return this + sum + log
}

class Host {
    var seen = ""
    fun String.bundle(vararg xs: Int = run { seen += "d"; intArrayOf(1, 2) }): String {
        var sum = 0
        for (value in xs) sum += value
        return this + sum + seen
    }
}
"#;

#[test]
fn classpath_extension_omitted_vararg_default_executes() {
    common::Fixture::new()
        .reference_lib("Lib.kt", LIB)
        .assert_box_ok(
            r#"
        import dep.pack
        fun box(): String {
            val omitted = "x".pack()
            val given = "y".pack(42)
            return if (omitted == "x3d" && given == "y42d") "OK" else "$omitted/$given"
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
        fun box(): String {
            val host = Host()
            val omitted = with(host) { "x".bundle() }
            val given = with(host) { "y".bundle(40, 2) }
            return if (omitted == "x3d" && given == "y42d") "OK" else "$omitted/$given"
        }
        "#,
        );
}
