//! A Kotlin `String` is a sequence of UTF-16 code UNITS, so a `\uXXXX` escape in the surrogate
//! range D800..DFFF is a legal element of a string constant. A Rust `String` holds Unicode SCALAR
//! values and cannot spell one, so every stage that carries a string constant — the literal
//! unescaper, the AST, the constant-string fold, the IR, and the class-file constant pool — has to
//! carry code units rather than `char`s. Expected values are kotlinc 2.4.10's.
use super::common;

/// Strict stdlib/JDK run: missing tooling or a rejected source panics with diagnostics.
fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "Main")
}

#[test]
fn escaped_surrogate_pair_literal_is_one_supplementary_character() {
    // `"\uD83D\uDE00"` is U+1F600 written as its two UTF-16 halves. Decoding each escape on its own
    // through a Unicode-scalar type rejects both halves; the constant then silently becomes "".
    const SRC: &str = "fun box(): String {\n\
        \x20 val s = \"\\uD83D\\uDE00\"\n\
        \x20 if (s.length != 2) return \"f0: \" + s.length\n\
        \x20 if (s != \"\\uD83D\\uDE00\") return \"f1\"\n\
        \x20 if (s[0].code != 55357) return \"f2: \" + s[0].code\n\
        \x20 if (s[1].code != 56832) return \"f3: \" + s[1].code\n\
        \x20 if (s.codePointAt(0) != 128512) return \"f4: \" + s.codePointAt(0)\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn lone_surrogate_literal_keeps_its_code_unit() {
    // An UNPAIRED surrogate is still a legal `String` element; kotlinc keeps it verbatim.
    const SRC: &str = "fun box(): String {\n\
        \x20 val s = \"x\\uD800y\"\n\
        \x20 if (s.length != 3) return \"f0: \" + s.length\n\
        \x20 if (s[1].code != 55296) return \"f1: \" + s[1].code\n\
        \x20 if (s[1] != Char.MIN_HIGH_SURROGATE) return \"f2\"\n\
        \x20 if (s[0] != 'x' || s[2] != 'y') return \"f3\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn the_classpath_less_trim_indent_fold_emits_the_surrogate() {
    // WITHOUT kotlin-stdlib on the classpath there is no resolved `trimIndent` target, so the lowerer
    // takes its own constant fold. That fold used to be unable to represent the receiver and the file
    // was REJECTED ("this construct is not yet supported by the IR backend"). Check the emitted pool
    // rather than running: the class has no stdlib to run against.
    const SRC: &str = "fun box(): String = \"\"\"${'\\uD800'}x\"\"\".trimIndent()\n";
    let classes = common::expect_compile_in_process(SRC, "Main", &[], None);
    let (_, bytes) = classes.first().expect("one class file");
    // Modified UTF-8 writes each UTF-16 code unit separately, so U+D800 is `ED A0 80` — the encoding
    // the JVM itself uses for an unpaired surrogate — followed by `x`.
    let needle = [0xEDu8, 0xA0, 0x80, b'x'];
    assert!(
        bytes.windows(needle.len()).any(|w| w == needle),
        "folded constant missing from the constant pool"
    );
}

#[test]
fn lone_surrogate_folds_through_trim_indent() {
    // `trimIndent`/`trimMargin` on a constant receiver is folded at lowering. A `Char` template part
    // with no Unicode-scalar form used to make the whole fold unrepresentable, and the file was then
    // REJECTED by the IR backend.
    const SRC: &str = "fun box(): String {\n\
        \x20 val s = \"\"\"${'\\uD800'}x\"\"\".trimIndent()\n\
        \x20 if (s.length != 2) return \"f0: \" + s.length\n\
        \x20 if (s[0].code != 55296) return \"f1: \" + s[0].code\n\
        \x20 if (s[1] != 'x') return \"f2\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn lone_surrogate_folds_through_trim_margin() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val s = \"\"\"\n\
        \x20     |a${'\\uDFFF'}\n\
        \x20     \"\"\".trimMargin()\n\
        \x20 if (s.length != 2) return \"f0: \" + s.length\n\
        \x20 if (s[0] != 'a') return \"f1\"\n\
        \x20 if (s[1] != Char.MAX_LOW_SURROGATE) return \"f2: \" + s[1].code\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn a_supplementary_constant_part_appends_as_a_string_not_a_char() {
    // The `StringBuilder` path appends a ONE-CHARACTER string constant as a `char` (kotlinc's form).
    // "one character" has to mean one code UNIT: a supplementary character is two units and does not
    // fit a `Char`, so pushing it as one would truncate it through `i2c`.
    const SRC: &str = "fun box(): String {\n\
        \x20 val n = \"\\uD83D\\uDE00\".length\n\
        \x20 val s = \"\\uD83D\\uDE00\" + n\n\
        \x20 if (s.length != 3) return \"f0: \" + s.length\n\
        \x20 if (s[0].code != 55357 || s[1].code != 56832) return \"f1\"\n\
        \x20 if (s[2] != '2') return \"f2\"\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn lone_surrogate_survives_a_const_val_and_a_template() {
    // A `const val` goes through the field `ConstantValue` attribute rather than an `ldc` in a body,
    // and a template part goes through `StringBuilder.append`/`makeConcatWithConstants`. Both write
    // the constant through the class-file pool's modified-UTF-8 encoder, which must emit the lone
    // surrogate as its own 3-byte sequence rather than dropping or replacing it.
    const SRC: &str = "const val HI = \"\\uD800\"\n\
        fun box(): String {\n\
        \x20 if (HI.length != 1) return \"f0: \" + HI.length\n\
        \x20 if (HI[0].code != 55296) return \"f1: \" + HI[0].code\n\
        \x20 val t = \"[${HI}]\"\n\
        \x20 if (t.length != 3) return \"f2: \" + t.length\n\
        \x20 if (t[1].code != 55296) return \"f3: \" + t[1].code\n\
        \x20 return \"OK\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn lone_surrogate_survives_classpath_constant_read_and_inline_relocation() {
    // This is intentionally a REAL module boundary: kotlinc writes the dependency's ConstantValue,
    // then krusty's generic class reader and library-constant contract must carry the original UTF-16
    // unit into the consumer's IR. The inline function exercises the other consumer of the SAME
    // decoded pool entry: bytecode relocation. A String-only read path changes either to U+FFFD.
    const LIB: &str =
        "package dep\nconst val MARK = \"\\uD800\"\ninline fun mark() = \"\\uD800\"\n";
    const MAIN: &str = "import dep.MARK\nimport dep.mark\n\
        fun box(): String {\n\
        \x20 if (MARK.length != 1) return \"f0: \" + MARK.length\n\
        \x20 if (MARK[0].code != 55296) return \"f1: \" + MARK[0].code\n\
        \x20 val fromInline = mark()\n\
        \x20 if (fromInline.length != 1) return \"f2: \" + fromInline.length\n\
        \x20 if (fromInline[0].code != 55296) return \"f3: \" + fromInline[0].code\n\
        \x20 return \"OK\"\n\
        }\n";

    let Some(output) = common::expect_box_run_against("utf16_classpath_const", LIB, MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}
