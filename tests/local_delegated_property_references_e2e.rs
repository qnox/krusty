//! kotlinc keeps a LOCAL delegated property's reflected property with its lexical class's other
//! delegated properties (`PropertyReferenceLowering`): one `$$delegatedProperties` slot, in source
//! order among the class's members, named `<v#N>` by its place among the class's local delegated
//! properties, owned by that class, and flagged `1` when the class is a file facade.
//!
//! Each case compares the classes' static initializers, which fill those arrays, with kotlinc's.
//! The fixtures use neutral names only.
use super::common;

/// The disassembled `static {}` of `class` in `dir`, with its constant-pool indices dropped: the
/// two compilers intern the same references in different orders.
fn static_initializer(dir: &std::path::Path, class: &str) -> String {
    let path = dir.join(format!("{class}.class"));
    let text = common::javap(&["-c", "-p", "-constants", &path.to_string_lossy()])
        .expect("the JVM is provisioned");
    text.lines()
        .skip_while(|line| line.trim() != "static {};")
        .take_while(|line| !line.trim().is_empty() && line.trim() != "}")
        .map(|line| {
            line.split_whitespace()
                .filter(|token| !token.starts_with('#'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compile `src` with kotlinc and krusty, and assert each class in `classes` fills its array as
/// kotlinc's does.
fn assert_same_arrays(stem: &str, src: &str, classes: &[&str]) {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("krusty");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    std::fs::create_dir_all(&krusty_dir).expect("krusty output directory");
    let source = dir.join(format!("{stem}.kt"));
    std::fs::write(&source, src).expect("write fixture");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let krusty = common::compile_in_process_metadata_cp_module_target(
        src,
        stem,
        &[common::stdlib_jar()],
        "main",
        None,
    )
    .expect("krusty compiles the fixture");
    for (internal, bytes) in &krusty {
        std::fs::write(krusty_dir.join(format!("{internal}.class")), bytes)
            .expect("write krusty class");
    }
    for class in classes {
        let reference = static_initializer(&reference_dir, class);
        assert!(
            reference.contains("<v#"),
            "{class}: the fixture must give it a local delegated property"
        );
        assert_eq!(
            static_initializer(&krusty_dir, class),
            reference,
            "{class} fills its array unlike kotlinc"
        );
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
fn local_delegated_properties_are_numbered_in_their_class_in_source_order() {
    let src = format!(
        "{CELL}\n\
         class Rack {{\n\
         \x20   val first: Int by Cell(0)\n\
         \x20   fun one(): Int {{\n\
         \x20       val left by Cell(1)\n\
         \x20       val read = {{ val inner by Cell(2); inner }}\n\
         \x20       return left + read()\n\
         \x20   }}\n\
         \x20   init {{\n\
         \x20       var counted by Cell(3)\n\
         \x20       counted = touch(counted)\n\
         \x20   }}\n\
         \x20   val second: Int by Cell(4)\n\
         \x20   val computed: Int\n\
         \x20       get() {{\n\
         \x20           val held by Cell(5)\n\
         \x20           return held\n\
         \x20       }}\n\
         \x20   fun two(): Int {{\n\
         \x20       val right by Cell(6)\n\
         \x20       return right\n\
         \x20   }}\n\
         }}\n"
    );
    assert_same_arrays("RackLocals", &src, &["Rack"]);
}

#[test]
fn an_anonymous_object_numbers_its_own_local_delegated_properties() {
    let src = format!(
        "{CELL}\n\
         class Shelf {{\n\
         \x20   fun make(): Int {{\n\
         \x20       val outer by Cell(1)\n\
         \x20       val made = object {{\n\
         \x20           fun read(): Int {{\n\
         \x20               val inner by Cell(2)\n\
         \x20               return inner\n\
         \x20           }}\n\
         \x20       }}\n\
         \x20       return outer + made.read()\n\
         \x20   }}\n\
         }}\n"
    );
    assert_same_arrays("ShelfLocals", &src, &["Shelf", "Shelf$make$made$1"]);
}

#[test]
fn a_top_level_local_delegated_property_belongs_to_its_file_facade() {
    let src = format!(
        "{CELL}\n\
         fun first(): Int {{\n\
         \x20   val left by Cell(1)\n\
         \x20   return left\n\
         }}\n\
         \n\
         fun second(): Int {{\n\
         \x20   var right by Cell(2)\n\
         \x20   right = touch(3)\n\
         \x20   return right\n\
         }}\n"
    );
    assert_same_arrays("FacadeLocals", &src, &["FacadeLocalsKt"]);
}
