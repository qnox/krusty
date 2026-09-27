//! Two smart-cast rules kotlinc applies to a local `var`. Its initializer does not narrow a declared
//! type: `var x: Any = 42` reads as `Any` until an assignment narrows it. And a catch can begin at
//! any point of its try body, so a variable the body reassigns loses its entry narrowing there.
//! Keeping either narrowing read a `String` through an `Integer` cast in the catch. A write is
//! matched to the binding it resolves to, so a shadowing inner declaration leaves the outer one alone.

use super::common;

const SOURCE: &str = "\
    class Failure : Throwable()\n\
    class Other : Throwable()\n\
    fun initialized(): String {\n\
    \x20   var x: Any = 42\n\
    \x20   return x.toString()\n\
    }\n\
    fun nested(): String {\n\
    \x20   var x: Any = 42\n\
    \x20   try {\n\
    \x20       try {\n\
    \x20           x = \"OK\"\n\
    \x20           throw Failure()\n\
    \x20       } catch (e: Other) {\n\
    \x20       }\n\
    \x20   } catch (e: Throwable) {\n\
    \x20       return x.toString()\n\
    \x20   }\n\
    \x20   return \"nested\"\n\
    }\n\
    fun reassigned(): String {\n\
    \x20   var x: Any = \"start\"\n\
    \x20   x = 7\n\
    \x20   try {\n\
    \x20       x = \"OK\"\n\
    \x20       throw Failure()\n\
    \x20   } catch (e: Failure) {\n\
    \x20       return x.toString()\n\
    \x20   }\n\
    }\n\
    ";

#[test]
fn a_declared_var_reads_as_its_declared_type_like_kotlinc() {
    let pair = common::ModuleClassPair::compile(&[("Flow.kt", SOURCE)], "FlowKt");
    let (kotlinc, krusty) = pair.method_code("FlowKt", "initialized");
    assert_eq!(krusty, kotlinc);
}

#[test]
fn a_catch_forgets_what_its_try_body_reassigns() {
    let source = format!(
        "{SOURCE}\
        fun box(): String {{\n\
        \x20   if (initialized() != \"42\") return \"initialized\"\n\
        \x20   if (nested() != \"OK\") return \"nested\"\n\
        \x20   return reassigned()\n\
        }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "CatchEntryFlow");
}

/// A narrowing made before the try and never rewritten in its body still holds in the catch.
#[test]
fn a_catch_keeps_what_its_try_body_leaves_alone() {
    let source = "\
        class Target { fun name(): String = \"OK\" }\n\
        fun kept(): String {\n\
        \x20   var x: Any = \"\"\n\
        \x20   x = Target()\n\
        \x20   try {\n\
        \x20       throw Exception()\n\
        \x20   } catch (e: Exception) {\n\
        \x20       return x.name()\n\
        \x20   }\n\
        }\n\
        fun box(): String = kept()\n\
        ";
    common::expect_box_same_as_kotlinc(source, "CatchKeptFlow");
}

/// kotlinc rejects a member of the entry narrowing in a catch whose try body reassigns the var, and
/// a member of the initializer's type on a declared `var`.
#[test]
fn lost_narrowings_are_rejected_like_kotlinc() {
    let source = "\
        class Target { fun name(): String = \"OK\" }\n\
        fun caught(): String {\n\
        \x20   var x: Any = \"\"\n\
        \x20   x = Target()\n\
        \x20   try {\n\
        \x20       x = \"text\"\n\
        \x20   } catch (e: Exception) {\n\
        \x20       return x.name()\n\
        \x20   }\n\
        \x20   return \"\"\n\
        }\n\
        fun declared(): String {\n\
        \x20   var y: Any = Target()\n\
        \x20   return y.name()\n\
        }\n\
        ";
    let result = common::compiler_diagnostics(
        &[("Lost.kt", source)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&result, "lost narrowings");
}

/// A write to a same-named inner declaration writes another binding: the outer narrowing still holds
/// in the catch.
#[test]
fn a_shadowing_write_in_the_try_body_keeps_the_outer_narrowing() {
    let source = "\
        class Box(val size: Int)\n\
        class Failure : Exception()\n\
        fun shadowed(fail: Boolean): Int {\n\
        \x20   var s: Any = \"\"\n\
        \x20   s = Box(7)\n\
        \x20   try {\n\
        \x20       if (fail) {\n\
        \x20           var s: Any = 1\n\
        \x20           s = 2\n\
        \x20       }\n\
        \x20       if (fail) throw Failure()\n\
        \x20   } catch (e: Failure) {\n\
        \x20       return s.size\n\
        \x20   }\n\
        \x20   return 1\n\
        }\n\
        ";
    let pair = common::ModuleClassPair::compile(&[("Shadow.kt", source)], "ShadowKt");
    let (kotlinc, krusty) = pair.method_code("ShadowKt", "shadowed");
    assert_eq!(krusty, kotlinc);
    let runnable = format!(
        "{source}fun box(): String = if (shadowed(true) == 7 && shadowed(false) == 1) \"OK\" else \"fail\"\n"
    );
    common::expect_box_same_as_kotlinc(&runnable, "CatchShadowFlow");
}
