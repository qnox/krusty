//! A reified `catch (e: E)` uses the inline call's type argument as its JVM catch type.
//!
//! The clause erases to `E`'s bound while the function is still generic. After inlining,
//! kotlinc catches that argument's class (`Exception`, `IllegalStateException`), so a thrown
//! supertype falls through to the outer handler.

use super::common;

#[test]
fn reified_catch_uses_the_call_type_argument() {
    common::expect_box_same_as_kotlinc(
        "// LANGUAGE: +AllowReifiedTypeInCatchClause\n\
         inline fun <reified E : Throwable> evalCatch(block: () -> Nothing): String {\n\
         \x20   return try {\n\
         \x20       try {\n\
         \x20           block()\n\
         \x20       } catch (ignore: E) {\n\
         \x20       }\n\
         \x20       \"Y\"\n\
         \x20   } catch (throwable: Throwable) {\n\
         \x20       \"N\"\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val log = evalCatch<Exception> { throw Throwable() } +\n\
         \x20       evalCatch<Exception> { throw Exception() } +\n\
         \x20       evalCatch<IllegalStateException> { throw Exception() } +\n\
         \x20       evalCatch<IllegalStateException> { throw IllegalStateException() }\n\
         \x20   return if (log == \"NYNY\") \"OK\" else log\n\
         }\n",
        "ReifiedCatchType",
    );
}
