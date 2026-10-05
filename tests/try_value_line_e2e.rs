//! A `try` used as a value stores its result in a temporary and reloads it after the last catch.
//! kotlinc's `visitVariable` marks the initializer's line before that reload, so `val x = try {`
//! returns to the declaration's line at the load rather than at the store. `visitSetValue` marks
//! the assignment's line before its store, so `x = try {` gets that line back after the catch.
//! krusty marked neither: the reload kept the last catch's line, and an assignment's store had no
//! line of its own.
use super::common;

#[test]
fn try_values_mark_their_declaration_and_assignment_lines_like_kotlinc() {
    let src = "package store\n\
               \n\
               fun risky(n: Int): Int = if (n > 0) n else throw IllegalStateException(\"n\")\n\
               \n\
               fun box(): String {\n\
               \x20   val a = try {\n\
               \x20       risky(5)\n\
               \x20   } catch (e: IllegalStateException) {\n\
               \x20       6\n\
               \x20   }\n\
               \x20   val b: Any = try {\n\
               \x20       risky(0)\n\
               \x20   } catch (e: IllegalStateException) {\n\
               \x20       \"caught\"\n\
               \x20   } finally {\n\
               \x20       risky(1)\n\
               \x20   }\n\
               \x20   val c = try { risky(2) } catch (e: Exception) { 0 }\n\
               \x20   var d = 0\n\
               \x20   d = try {\n\
               \x20       risky(3)\n\
               \x20   } catch (e: IllegalStateException) {\n\
               \x20       4\n\
               \x20   }\n\
               \x20   val e = (try {\n\
               \x20       risky(-1)\n\
               \x20   } catch (t: Throwable) {\n\
               \x20       7\n\
               \x20   })\n\
               \x20   return if (a == 5 && b == \"caught\" && c == 2 && d == 3 && e == 7) \"OK\" else \"FAIL\"\n\
               }\n";
    common::byte_diff_against_kotlinc_cp(
        "TryValueLine",
        src,
        "store/TryValueLineKt",
        &[common::stdlib_jar(), common::jdk_modules()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("store/TryValueLineKt byte-identical to kotlinc");
}
