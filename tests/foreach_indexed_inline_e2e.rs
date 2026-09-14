//! Checked forEachIndexed expansion, including its loop-carried index across suspension.

use super::common;

const ITERABLE_SOURCE: &str = "import kotlinx.coroutines.runBlocking\n\
    suspend fun twice(value: Int): Int = value * 2\n\
    fun box(): String = runBlocking {\n\
    \x20   val seen = StringBuilder()\n\
    \x20   listOf(10, 20, 30).forEachIndexed { index, value ->\n\
    \x20       if (value == 20) return@forEachIndexed\n\
    \x20       seen.append(index).append(':').append(twice(value)).append(';')\n\
    \x20   }\n\
    \x20   if (seen.toString() == \"0:20;2:60;\") \"OK\" else \"F:$seen\"\n\
    }\n";

const ITERATION_FAMILIES_SOURCE: &str = "import kotlinx.coroutines.runBlocking\n\
    suspend fun tick(value: Int): Int = value\n\
    class Letters(private val text: String) : CharSequence {\n\
    \x20   override val length: Int get() = text.length\n\
    \x20   override fun get(index: Int): Char = text[index]\n\
    \x20   override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
    \x20       text.subSequence(startIndex, endIndex)\n\
    }\n\
    fun box(): String = runBlocking {\n\
    \x20   var score = 0\n\
    \x20   arrayOf(1, 2).forEachIndexed { i, v -> score += tick(i) + v }\n\
    \x20   booleanArrayOf(true, false).forEachIndexed { i, v -> score += tick(i) + if (v) 1 else 0 }\n\
    \x20   byteArrayOf(1, 2).forEachIndexed { i, v -> score += tick(i) + v.toInt() }\n\
    \x20   shortArrayOf(1, 2).forEachIndexed { i, v -> score += tick(i) + v.toInt() }\n\
    \x20   intArrayOf(1, 2).forEachIndexed { i, v -> score += tick(i) + v }\n\
    \x20   longArrayOf(1, 2).forEachIndexed { i, v -> score += tick(i) + v.toInt() }\n\
    \x20   floatArrayOf(1f, 2f).forEachIndexed { i, v -> score += tick(i) + v.toInt() }\n\
    \x20   doubleArrayOf(1.0, 2.0).forEachIndexed { i, v -> score += tick(i) + v.toInt() }\n\
    \x20   charArrayOf('a', 'b').forEachIndexed { i, v -> score += tick(i) + v.code }\n\
    \x20   \"cd\".forEachIndexed { i, v -> score += tick(i) + v.code }\n\
    \x20   (Letters(\"ef\") as CharSequence).forEach { score += tick(it.code) }\n\
    \x20   mapOf(\"q\" to 7).forEach { (key, value) -> score += key.length + tick(value) }\n\
    \x20   if (score == 637) \"OK\" else \"F:$score\"\n\
    }\n";

const MUTABLE_CHAR_SEQUENCE_SOURCE: &str =
    "class Shrinking(private val text: String) : CharSequence {\n\
    \x20   var limit: Int = text.length\n\
    \x20   override val length: Int get() = limit\n\
    \x20   override fun get(index: Int): Char {\n\
    \x20       val result = text[index]\n\
    \x20       limit--\n\
    \x20       return result\n\
    \x20   }\n\
    \x20   override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
    \x20       text.subSequence(startIndex, endIndex)\n\
    }\n\
    fun box(): String {\n\
    \x20   val seen = StringBuilder()\n\
    \x20   (Shrinking(\"abc\") as CharSequence).forEach { seen.append(it) }\n\
    \x20   return if (seen.toString() == \"ab\") \"OK\" else \"F:$seen\"\n\
    }\n";

fn krusty_result(source: &str) -> String {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    common::expect_box_run(
        source,
        "Main",
        &[stdlib, coroutines, jdk.clone()],
        Some(jdk.as_path()),
    )
}

fn kotlinc_result(source_text: &str) -> String {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    let scratch = common::scratch_dir().expect("scratch directory");
    let source = scratch.join("Main.kt");
    let output = scratch.join("reference");
    std::fs::write(&source, source_text).expect("write reference source");
    std::fs::create_dir_all(&output).expect("create reference output directory");
    let classpath = std::env::join_paths([stdlib.as_path(), coroutines.as_path()])
        .expect("reference classpath")
        .to_string_lossy()
        .into_owned();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-cp".to_string(),
        classpath,
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");
    assert_eq!(code, 0, "kotlinc must accept the runtime fixture: {stderr}");
    let result = common::run_box(&[], "MainKt", &[output, stdlib, coroutines, jdk])
        .expect("run kotlinc-built forEachIndexed fixture");
    let _ = std::fs::remove_dir_all(scratch);
    result
}

#[test]
fn continue_advances_the_index_and_runtime_matches_kotlinc() {
    let reference = kotlinc_result(ITERABLE_SOURCE);
    assert_eq!(reference, "OK", "the oracle must exercise indexes 0 and 2");
    assert_eq!(krusty_result(ITERABLE_SOURCE), reference);
}

#[test]
fn every_iteration_family_with_suspension_matches_kotlinc() {
    let reference = kotlinc_result(ITERATION_FAMILIES_SOURCE);
    assert_eq!(reference, "OK", "the oracle must exercise every family");
    assert_eq!(krusty_result(ITERATION_FAMILIES_SOURCE), reference);
}

#[test]
fn char_sequence_length_remains_the_compiled_loop_condition() {
    let reference = kotlinc_result(MUTABLE_CHAR_SEQUENCE_SOURCE);
    assert_eq!(
        reference, "OK",
        "the oracle must observe the changing length"
    );
    assert_eq!(krusty_result(MUTABLE_CHAR_SEQUENCE_SOURCE), reference);
}

#[test]
fn separately_compiled_same_package_iterator_cannot_rebind_an_inline_body() {
    const LIB: &str = "package p\n\
        inline fun CharSequence.each(action: (Char) -> Unit) {\n\
        \x20   for (element in this) action(element)\n\
        }\n";
    const SHADOW: &str = "package p\n\
        object EmptyChars : CharIterator() {\n\
        \x20   override fun hasNext(): Boolean = false\n\
        \x20   override fun nextChar(): Char = error(\"empty\")\n\
        }\n\
        operator fun CharSequence.iterator(): CharIterator = EmptyChars\n";
    const MAIN: &str = "package client\n\
        import p.each\n\
        fun box(): String {\n\
        \x20   val seen = StringBuilder()\n\
        \x20   \"ab\".each { seen.append(it) }\n\
        \x20   return if (seen.toString() == \"ab\") \"OK\" else \"F:$seen\"\n\
        }\n";

    let library = common::kotlinc_library(LIB).expect("kotlinc compiles the inline dependency");
    let shadow = common::kotlinc_library(SHADOW).expect("kotlinc compiles the shadow dependency");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = vec![library, shadow, stdlib];

    let reference_scratch = common::scratch_dir().expect("reference scratch directory");
    let reference_source = reference_scratch.join("Main.kt");
    let reference_output = reference_scratch.join("classes");
    std::fs::write(&reference_source, MAIN).expect("write reference consumer");
    std::fs::create_dir_all(&reference_output).expect("create reference output");
    let joined = std::env::join_paths(&classpath)
        .expect("join reference classpath")
        .to_string_lossy()
        .into_owned();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_output.to_string_lossy().into_owned(),
        "-cp".to_string(),
        joined,
        reference_source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");
    assert_eq!(code, 0, "kotlinc must accept the consumer: {stderr}");
    let reference = common::run_box(
        &[],
        "client.MainKt",
        &[
            reference_output,
            classpath[0].clone(),
            classpath[1].clone(),
            classpath[2].clone(),
            jdk.clone(),
        ],
    )
    .expect("run kotlinc-built consumer");
    assert_eq!(
        reference, "OK",
        "the compiled body must ignore the later shadow"
    );
    assert_eq!(
        common::expect_box_run(MAIN, "Main", &classpath, Some(jdk.as_path())),
        reference
    );
    let _ = std::fs::remove_dir_all(reference_scratch);
}
