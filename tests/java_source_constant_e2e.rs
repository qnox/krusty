//! A `static final` Java source field initialized by one literal is a constant variable (JLS
//! §4.12.4), so a Kotlin read of it compiles to the constant, as it does against the javac-built
//! class. The Android Gradle plugin's generated `BuildConfig` (`DEBUG`, `VERSION_CODE`) is that
//! shape, and it reaches the Kotlin compile as a Java source.

use super::common;

#[test]
fn a_java_source_literal_constant_is_inlined_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        (
            "Reads.kt",
            "import p.Config\n\
             \n\
             fun debug(): Boolean = Config.DEBUG\n\
             fun code(): Int = Config.CODE\n\
             fun negative(): Int = Config.NEGATIVE\n\
             fun mask(): Int = Config.MASK\n\
             fun stamp(): Long = Config.STAMP\n\
             fun widened(): Long = Config.WIDENED\n\
             fun letter(): Char = Config.LETTER\n\
             fun escaped(): Char = Config.ESCAPED\n\
             fun ratio(): Float = Config.RATIO\n\
             fun scale(): Double = Config.SCALE\n\
             fun small(): Short = Config.SMALL\n\
             fun tiny(): Byte = Config.TINY\n\
             fun name(): String = Config.NAME\n\
             fun mutable(): Boolean = Config.mutable\n",
        ),
        (
            "p/Config.java",
            "package p;\n\
             \n\
             public final class Config {\n\
             \x20   public static final boolean DEBUG = false;\n\
             \x20   public static final int CODE = 3;\n\
             \x20   public static final int NEGATIVE = -2147483648;\n\
             \x20   public static final int MASK = 0xFFFF_FFFF;\n\
             \x20   public static final long STAMP = 1_700_000_000_000L;\n\
             \x20   public static final long WIDENED = 'A';\n\
             \x20   public static final char LETTER = 'k';\n\
             \x20   public static final char ESCAPED = '\\n';\n\
             \x20   public static final float RATIO = 0.1f;\n\
             \x20   public static final double SCALE = -1.5e3;\n\
             \x20   public static final short SMALL = 300;\n\
             \x20   public static final byte TINY = -128;\n\
             \x20   public static final String NAME = \"release\";\n\
             \x20   public static boolean mutable = false;\n\
             }\n",
        ),
    ]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
