//! `@JvmName` on a top-level function names the emitted method. The SOURCE name still resolves the
//! call, so every path that reaches the declaration must emit the annotated spelling: a same-file
//! direct call, a callable reference (whose invoke targets the JVM name), and a cross-file call —
//! whose caller cannot see the callee's AST and so receives the compact resolved annotation header
//! attached to the stable callable identity.
//!
//! (`KFunction.name` is not resolvable yet, so the reflection half of the reference — which keeps
//! the Kotlin spelling — is not asserted here.)

use super::common;

#[test]
fn jvm_name_is_emitted_for_every_call_path() {
    let sources = [
        (
            "lib.kt",
            "@JvmName(\"renamedTop\")\n\
             fun top(): Int = 7\n\
             @JvmName(\"renamedArg\")\n\
             fun withArg(x: Int): Int = x + 1\n\
             fun sameFile(): Int = top() + withArg(1)\n\
             fun viaRef(): Int {\n\
             \x20 val r = ::top\n\
             \x20 return r()\n\
             }\n",
        ),
        (
            "main.kt",
            "fun box(): String {\n\
             \x20 if (sameFile() != 9) return \"f1:\" + sameFile()\n\
             \x20 if (top() != 7) return \"f2\"\n\
             \x20 if (withArg(4) != 5) return \"f3\"\n\
             \x20 if (viaRef() != 7) return \"f4\"\n\
             \x20 return \"OK\"\n\
             }\n",
        ),
    ];
    common::expect_box_ok_files_with_stdlib(&sources, "JvmNameTopLevel");
}

#[test]
fn jvm_name_separates_two_overloads_that_erase_alike() {
    // `g(String)` and `g(String?)` erase to one JVM signature, so kotlinc rejects the pair as a
    // platform declaration clash unless `@JvmName` renames one — and then both must still be
    // SELECTED by nullability from the source name, and both must be emitted and callable.
    let src = "fun g(x: String): String = \"nn:\" + x\n\
               @JvmName(\"gNullable\")\n\
               fun g(x: String?): String = \"nl:\" + (x ?: \"null\")\n\
               fun box(): String {\n\
               \x20 val a: String = \"a\"\n\
               \x20 val b: String? = null\n\
               \x20 if (g(a) != \"nn:a\") return \"f1|\" + g(a)\n\
               \x20 if (g(b) != \"nl:null\") return \"f2|\" + g(b)\n\
               \x20 return \"OK\"\n\
               }\n";
    common::expect_box_ok_with_stdlib(src, "JvmNameOverload");
}
