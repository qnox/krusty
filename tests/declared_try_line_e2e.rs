//! A local declaration whose initializer is a `try` marks the `try`'s line again on the load of the
//! `try`'s saved value, as kotlinc's `visitVariable` marks the initializer before materializing it.
//! The store then carries the declaration's line when it differs. An assignment does not.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
fun pick(a: Int): Int {\n\
    val x = try {\n\
        a / 2\n\
    } catch (e: ArithmeticException) {\n\
        6\n\
    }\n\
    val y =\n\
        try {\n\
            a / 3\n\
        } catch (e: ArithmeticException) {\n\
            7\n\
        }\n\
    val s: String = try {\n\
        \"a\" + a\n\
    } finally {\n\
        a + 1\n\
    }\n\
    var z = 0\n\
    z = try {\n\
        a / 4\n\
    } catch (e: ArithmeticException) {\n\
        8\n\
    }\n\
    return x + y + s.length + z\n\
}\n\
";

#[test]
fn declared_try_reload_marks_the_initializer_line() {
    common::byte_diff_against_kotlinc_cp(
        "DeclaredTryLine",
        SOURCE,
        "store/DeclaredTryLineKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("declared try lines differ from kotlinc: {diff}"));
}
