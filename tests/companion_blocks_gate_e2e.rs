//! `CompanionBlocksAndExtensions` (`-Xcompanion-blocks-and-extensions`). Without the feature kotlinc
//! reports `UNSUPPORTED_FEATURE` at the `companion` keyword of a classifier's first `companion { … }`
//! block, at a written companion extension's `companion` modifier (which a file-level declaration
//! may not carry either), and at the name of every reference that selects a block member: a call, a
//! property read, unqualified inside the class or qualified by it, and a callable reference. It does
//! not look for written companion extensions then, so a reference to one is unresolved. The complete
//! error ledger must be the expected one for both compilers, and with the argument both run the
//! fixture to "OK".

use super::common;

const UNSUPPORTED: &str = "the feature \"companion blocks and extensions\" is experimental and \
    should be enabled explicitly. This can be done by supplying the compiler argument \
    '-Xcompanion-blocks-and-extensions', but note that no stability guarantees are provided.";
const FILE_MODIFIER: &str = "modifier 'companion' is not applicable inside 'file'.";

const COMPANIONS: &str = r#"class C {
    companion {
        val p: String = "p"
        fun f(): String = "f"
    }
    companion {
        fun g() = 1
    }
    fun inside() = p + f()
}
class D {
    companion {
        fun h() = 2
    }
}
companion fun C.ext(): String = "e"
companion val C.extP: String get() = "x"
fun box(): String {
    val r = C::f
    val all = C.p + C.f() + C.g() + D.h() + C.ext() + C.extP + C().inside()
    return if (all == "pf12expf") "OK" else "fail: $all"
}
"#;

#[test]
fn companion_blocks_and_extensions_require_the_feature() {
    let error = |at: &str, message: &str| format!("Main.kt:{at}: {message}");
    let expected = [
        error("2:5", UNSUPPORTED),
        error("9:20", UNSUPPORTED),
        error("9:24", UNSUPPORTED),
        error("12:5", UNSUPPORTED),
        error("16:1", UNSUPPORTED),
        error("16:1", FILE_MODIFIER),
        error("17:1", UNSUPPORTED),
        error("17:1", FILE_MODIFIER),
        error("19:16", UNSUPPORTED),
        error("20:17", UNSUPPORTED),
        error("20:23", UNSUPPORTED),
        error("20:31", UNSUPPORTED),
        error("20:39", UNSUPPORTED),
        error("20:47", "unresolved reference 'ext'."),
        error("20:57", "unresolved reference 'extP'."),
    ];
    let sources = [("Main.kt", COMPANIONS)];
    assert_eq!(
        common::reference_error_ledger(&sources, &[]),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::krusty_error_ledger_with_args(&sources, &[]),
        expected
    );
}

#[test]
fn companion_blocks_and_extensions_run_with_the_argument() {
    common::expect_box_same_as_kotlinc_with_args(
        COMPANIONS,
        "CompanionBlocks",
        &["-Xcompanion-blocks-and-extensions"],
    );
}
