//! kotlinc keeps the `KProperty` references its delegated-property operators receive in one
//! `$$delegatedProperties` array per class (`PropertyReferenceLowering`): a leading
//! `static final synthetic` field, filled first in `<clinit>` in declaration order, read as
//! `$$delegatedProperties[i]`. Such a `<clinit>` has no line table, and a delegate's constructor
//! store maps to its property's line.
//!
//! Each case asserts that the named classes are byte-identical to kotlinc's. The fixtures use
//! neutral names only.
use super::common;

/// Compile `src` with kotlinc and krusty, and assert each class in `classes` is byte-identical.
fn assert_identical(stem: &str, src: &str, classes: &[&str]) {
    assert_identical_against(stem, src, classes, &[]);
}

/// [`assert_identical`] with `libraries` on both compilers' classpath.
fn assert_identical_against(
    stem: &str,
    src: &str,
    classes: &[&str],
    libraries: &[std::path::PathBuf],
) {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference_dir = dir.join("ref");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    let source = dir.join(format!("{stem}.kt"));
    std::fs::write(&source, src).expect("write fixture");
    let mut arguments = vec![
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
    ];
    if !libraries.is_empty() {
        let classpath = std::env::join_paths(libraries).expect("classpath");
        arguments.push("-cp".to_string());
        arguments.push(classpath.to_string_lossy().into_owned());
    }
    arguments.push(source.to_string_lossy().into_owned());
    let (code, stderr) =
        common::kotlinc_compile(&arguments).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let mut classpath = vec![common::stdlib_jar()];
    classpath.extend_from_slice(libraries);
    let krusty =
        common::compile_in_process_metadata_cp_module_target(src, stem, &classpath, "main", None)
            .expect("krusty compiles the fixture");
    for class in classes {
        let reference = std::fs::read(reference_dir.join(format!("{class}.class")))
            .expect("kotlinc emits the class");
        let ours = krusty
            .iter()
            .find(|(internal, _)| internal == class)
            .map(|(_, bytes)| bytes)
            .expect("krusty emits the class");
        assert!(&reference == ours, "{class} differs from kotlinc's build");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

const CELL: &str = "import kotlin.reflect.KProperty\n\
     \n\
     class Cell(val stored: Int) {\n\
     \x20   operator fun getValue(owner: Any?, property: KProperty<*>): Int = stored\n\
     \x20   operator fun setValue(owner: Any?, property: KProperty<*>, value: Int) {}\n\
     }\n\
     \n\
     fun touch(value: Int): Int = value\n";

#[test]
fn a_class_keeps_its_delegated_property_references_in_one_array() {
    let src = format!(
        "{CELL}\n\
         class Shelf {{\n\
         \x20   val left: Int by Cell(1)\n\
         \x20   var right: Int by Cell(2)\n\
         \x20   val plain: Int = touch(3)\n\
         }}\n\
         \n\
         class Crate {{\n\
         \x20   val inner: Int by Cell(4)\n\
         \x20   companion object {{\n\
         \x20       val shared: Int = touch(5)\n\
         \x20   }}\n\
         }}\n"
    );
    assert_identical("ShelfArray", &src, &["Shelf", "Crate", "Crate$Companion"]);
}

#[test]
fn an_object_fills_its_array_before_its_instance() {
    let src = format!(
        "{CELL}\n\
         object Rack {{\n\
         \x20   val first: Int = touch(1)\n\
         \x20   val slot: Int by Cell(2)\n\
         \x20   var spare: Int by Cell(3)\n\
         \x20   init {{\n\
         \x20       touch(4)\n\
         \x20   }}\n\
         }}\n"
    );
    assert_identical("RackArray", &src, &["Rack"]);
}

#[test]
fn an_enum_fills_its_array_before_its_entries() {
    let src = format!(
        "{CELL}\n\
         enum class Tier {{\n\
         \x20   LOW, HIGH;\n\
         \x20   val weight: Int by Cell(1)\n\
         }}\n"
    );
    assert_identical("TierArray", &src, &["Tier"]);
}

#[test]
fn an_enum_declares_its_array_after_its_companion() {
    let src = format!(
        "{CELL}\n\
         enum class Grade {{\n\
         \x20   LOW, HIGH;\n\
         \x20   val weight: Int by Cell(1)\n\
         \x20   companion object {{\n\
         \x20       fun shared(): Int = touch(2)\n\
         \x20   }}\n\
         }}\n"
    );
    assert_identical("GradeArray", &src, &["Grade", "Grade$Companion"]);
}

#[test]
fn a_file_facade_keeps_its_top_level_references_in_one_array() {
    let src = format!(
        "{CELL}\n\
         val first: Int by Cell(1)\n\
         var second: Int by Cell(2)\n\
         val plain: Int = touch(3)\n"
    );
    assert_identical("FacadeArray", &src, &["FacadeArrayKt"]);
}

/// A current-module inline operator that never reads its property is passed `null`, so the
/// property takes no slot, while one that reads it keeps its slot. Only a local delegated
/// property's convention call is expanded in the module today, and its accessors are not yet
/// lifted like kotlinc's, so this runs the code rather than comparing its bytes.
#[test]
fn a_module_inline_operator_that_never_reads_its_property_runs() {
    let src = "import kotlin.reflect.KProperty\n\
         \n\
         class Quiet(val stored: Int)\n\
         inline operator fun Quiet.getValue(owner: Any?, property: KProperty<*>): Int = stored\n\
         \n\
         class Loud(val stored: Int)\n\
         inline operator fun Loud.getValue(owner: Any?, property: KProperty<*>): Int =\n\
         \x20   stored + property.name.length\n\
         \n\
         fun box(): String {\n\
         \x20   val silent by Quiet(1)\n\
         \x20   val named by Loud(2)\n\
         \x20   return if (silent + named == 8) \"OK\" else \"fail\"\n\
         }\n";
    common::expect_box_ok_with_stdlib(src, "ModuleInlineArray");
}

const INLINE_OPERATORS: &str = "package holders\n\
     import kotlin.reflect.KProperty\n\
     \n\
     class Forced(val stored: Int)\n\
     inline operator fun <reified T> Forced.getValue(owner: T, property: KProperty<*>): Int =\n\
     \x20   stored\n\
     \n\
     class Optional(val stored: Int)\n\
     inline operator fun Optional.getValue(owner: Any?, property: KProperty<*>?): Int = stored\n\
     \n\
     class Checked(val stored: Int)\n\
     inline operator fun Checked.getValue(owner: Any?, property: KProperty<*>): Int = stored\n";

/// A dependency's inline operator that never reads its property is passed `null`, as kotlinc
/// inlines it: one that takes a nullable property, and one that only checks its non-null property,
/// a check inlining removes. The class then needs no array at all.
#[test]
fn a_dependency_inline_operator_that_never_reads_its_property_takes_no_slot() {
    let library = common::kotlinc_library(INLINE_OPERATORS)
        .expect("reference compiler builds the inline operators");
    let src = "import holders.*\n\
         \n\
         class Holder {\n\
         \x20   val optional: Int by Optional(2)\n\
         \x20   val checked: Int by Checked(3)\n\
         }\n";
    assert_identical_against("DependencyInlineArray", src, &["Holder"], &[library]);
}

/// A reified operator must be inlined; passed `null` for the property it never reads, it runs.
/// Its inlined frame is not yet kotlinc's, so this runs the code rather than comparing its bytes.
#[test]
fn a_reified_dependency_operator_that_never_reads_its_property_runs() {
    let main = "import holders.*\n\
         \n\
         class Holder {\n\
         \x20   val forced: Int by Forced(1)\n\
         }\n\
         \n\
         fun box(): String = if (Holder().forced == 1) \"OK\" else \"fail\"\n";
    let result = common::expect_box_run_against_kotlinc(INLINE_OPERATORS, main)
        .expect("reference compiler builds the inline operators");
    assert_eq!(result, "OK");
}
