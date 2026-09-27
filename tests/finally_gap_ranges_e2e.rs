//! A transfer that inlines a `finally` leaves every `try` nested inside that finalizer's `try`, so
//! the copy lies outside all of their protected ranges, a catch-only `try` included (kotlinc's
//! KT-31923 rule): a throw from the copy reaches neither the inner catch nor the finalizer again.
//! A `try` enclosing the finalizer's own `try` still guards the copy.

use super::common;

const SOURCE: &str = "var log = 0\n\
    fun step(n: Int) { log = log * 10 + n }\n\
    fun more(): Boolean = log < 100\n\
    class Stop : Throwable()\n\
    fun returnThroughCatch() {\n\
    \x20   try {\n\
    \x20       try { step(1); return } catch (e: Throwable) { step(2) }\n\
    \x20   } finally { step(3); throw Stop() }\n\
    }\n\
    fun breakThroughCatch() {\n\
    \x20   while (more()) {\n\
    \x20       try {\n\
    \x20           try { step(1); break } catch (e: Throwable) { step(2) }\n\
    \x20       } finally { step(3); throw Stop() }\n\
    \x20   }\n\
    }\n\
    fun twoFinalizers(): Int {\n\
    \x20   try {\n\
    \x20       try {\n\
    \x20           try { step(1); return 1 } catch (e: Throwable) { step(2) }\n\
    \x20       } finally { step(3) }\n\
    \x20   } finally { step(4) }\n\
    \x20   return 2\n\
    }\n\
    fun nestedCatches() {\n\
    \x20   try {\n\
    \x20       try {\n\
    \x20           try { step(1); return } catch (e: Stop) { step(2) }\n\
    \x20       } catch (e: Throwable) { step(3) }\n\
    \x20       step(4)\n\
    \x20   } finally { step(5) }\n\
    }\n\
    fun divergingInner() {\n\
    \x20   try {\n\
    \x20       try {\n\
    \x20           try { step(1); return } catch (e: Throwable) { step(2) }\n\
    \x20       } finally { step(3); throw Stop() }\n\
    \x20   } finally { step(4); throw Stop() }\n\
    }\n\
    fun catchOutsideFinally() {\n\
    \x20   while (more()) {\n\
    \x20       try {\n\
    \x20           try { step(1); break } finally { step(2) }\n\
    \x20       } catch (e: Throwable) { step(3) }\n\
    \x20   }\n\
    }\n";

#[test]
fn inlined_finalizers_split_nested_catch_ranges_like_kotlinc() {
    let pair = common::ModuleClassPair::compile(&[("Gaps.kt", SOURCE)], "GapsKt");
    for method in [
        "returnThroughCatch",
        "breakThroughCatch",
        "twoFinalizers",
        "nestedCatches",
        "divergingInner",
        "catchOutsideFinally",
    ] {
        let (kotlinc, krusty) = pair.method_code("GapsKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

/// The exception a finalizer copy throws escapes the catch-only `try` the transfer left: the inner
/// catch never runs, and neither does the finalizer a second time.
#[test]
fn a_throwing_finalizer_copy_skips_the_catches_it_left() {
    let source = format!(
        "{SOURCE}\
        fun thrown(action: Int): Int {{\n\
        \x20   log = 0\n\
        \x20   try {{\n\
        \x20       if (action == 0) returnThroughCatch()\n\
        \x20       if (action == 1) breakThroughCatch()\n\
        \x20       if (action == 2) divergingInner()\n\
        \x20   }} catch (e: Stop) {{ return log }}\n\
        \x20   return -1\n\
        }}\n\
        fun box(): String {{\n\
        \x20   if (thrown(0) != 13) return \"return: \" + thrown(0)\n\
        \x20   if (thrown(1) != 13) return \"break: \" + thrown(1)\n\
        \x20   if (thrown(2) != 134) return \"diverging: \" + thrown(2)\n\
        \x20   log = 0\n\
        \x20   if (twoFinalizers() != 1 || log != 134) return \"two: \" + log\n\
        \x20   log = 0\n\
        \x20   nestedCatches()\n\
        \x20   if (log != 15) return \"nested: \" + log\n\
        \x20   return \"OK\"\n\
        }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "Gaps");
}
