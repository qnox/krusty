//! A Java static field is a compile-time constant only when it is `final` (JLS §4.12.4), whatever
//! its `ConstantValue` attribute says. The Android Gradle plugin's compile `R.jar` declares every
//! resource id `public static int` with a `ConstantValue` placeholder, so that ids stay overridable
//! across modules: kotlinc reads such a field with `getstatic` and inlines only a final field's value.

use krusty::jvm::classfile::{ClassWriter, ACC_FINAL, ACC_PUBLIC, ACC_STATIC, ACC_SUPER};

use super::common;

/// `ids.Ids`: a non-final static `MAIN` and a final static `FIXED`, each with a `ConstantValue`.
fn ids_class() -> Vec<u8> {
    let mut class = ClassWriter::new("ids/Ids", "java/lang/Object");
    class.set_access(ACC_PUBLIC | ACC_FINAL | ACC_SUPER);
    let main = class.const_int(0x7f01_0000);
    class.add_field_const(ACC_PUBLIC | ACC_STATIC, "MAIN", "I", main);
    let fixed = class.const_int(3);
    class.add_field_const(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, "FIXED", "I", fixed);
    class.finish()
}

#[test]
fn only_a_final_java_static_reads_as_its_constant_value() {
    const SRC: &str = "import ids.Ids\n\
                       fun main(): Int = Ids.MAIN\n\
                       fun fixed(): Int = Ids.FIXED\n";
    let dir = common::scratch_dir()
        .expect("a scratch directory")
        .join("java-nonfinal-constant");
    let _ = std::fs::remove_dir_all(&dir);
    let classes = dir.join("classes");
    std::fs::create_dir_all(classes.join("ids")).expect("create the class directory");
    std::fs::write(classes.join("ids/Ids.class"), ids_class()).expect("write ids/Ids.class");
    let source = dir.join("Main.kt");
    std::fs::write(&source, SRC).expect("write the source");

    let reference = dir.join("kotlinc");
    let (code, diagnostics) = common::kotlinc_compile(&[
        "-cp".to_string(),
        classes.display().to_string(),
        "-d".to_string(),
        reference.display().to_string(),
        source.display().to_string(),
    ])
    .expect("the reference compiler runs");
    assert_eq!(code, 0, "kotlinc: {diagnostics}");

    let compiled = common::compile_in_process(
        SRC,
        "Main",
        &[classes, common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty compiles");
    let (_, facade) = compiled
        .iter()
        .find(|(name, _)| name == "MainKt")
        .expect("krusty emits MainKt");
    assert_eq!(
        *facade,
        std::fs::read(reference.join("MainKt.class")).expect("kotlinc emits MainKt"),
        "MainKt differs from kotlinc's"
    );
}
