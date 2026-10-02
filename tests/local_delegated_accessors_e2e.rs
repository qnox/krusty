//! kotlinc reads and writes a LOCAL delegated property through accessors it lifts like local
//! functions: `<container>$lambda$N`, private static, taking the delegate (and a setter's value),
//! declared on the property's line and emitted even when nothing reads the property. A lambda
//! calls them with the delegate it captures; a local class or anonymous object keeps the delegate
//! in a `$x$delegate` field and calls them through `access$` bridges. A statement `x++` keeps the
//! value it read in a temporary, `--x` reads the property again, and `dec` adds `-1`. Kotlin
//! metadata lists each class's and facade's local delegated properties in `<v#N>` order.
//!
//! Each case asserts that the named classes are byte-identical to kotlinc's. The fixtures use
//! neutral names only.
use super::common;

const CELL: &str = "import kotlin.reflect.KProperty\n\
     \n\
     class Cell(var stored: Int) {\n\
     \x20   operator fun getValue(owner: Any?, property: KProperty<*>): Int = stored\n\
     \x20   operator fun setValue(owner: Any?, property: KProperty<*>, value: Int) {\n\
     \x20       stored = value\n\
     \x20   }\n\
     }\n";

#[test]
fn local_delegated_properties_are_read_and_written_through_lifted_accessors() {
    let src = format!(
        "{CELL}\n\
         class Counter {{\n\
         \x20   fun post(): Int {{ var a by Cell(1); a++; return 0 }}\n\
         \x20   fun pre(): Int {{ var a by Cell(1); ++a; return 0 }}\n\
         \x20   fun postValue(): Int {{ var a by Cell(1); return a++ }}\n\
         \x20   fun preValue(): Int {{ var a by Cell(1); return ++a }}\n\
         \x20   fun down(): Int {{ var a by Cell(1); a--; --a; return 0 }}\n\
         \x20   fun shared(): Int {{\n\
         \x20       var a by Cell(1)\n\
         \x20       a = a + 1\n\
         \x20       val read = {{ a }}\n\
         \x20       return read()\n\
         \x20   }}\n\
         \x20   fun unread(): Int {{\n\
         \x20       val u by Cell(2)\n\
         \x20       return 1\n\
         \x20   }}\n\
         }}\n"
    );
    common::assert_classes_identical_to_kotlinc("CounterLocals", &src, &["Cell", "Counter"]);
}

#[test]
fn local_classes_and_objects_keep_the_delegate_they_read() {
    let src = format!(
        "{CELL}\n\
         class Shelf {{\n\
         \x20   fun make(): Int {{\n\
         \x20       val a by Cell(1)\n\
         \x20       class Slot {{ fun read() = a }}\n\
         \x20       val made = object {{ fun read() = a }}\n\
         \x20       return Slot().read() + made.read()\n\
         \x20   }}\n\
         }}\n"
    );
    common::assert_classes_identical_to_kotlinc(
        "ShelfLocals",
        &src,
        &["Shelf", "Shelf$make$Slot", "Shelf$make$made$1"],
    );
}

#[test]
fn a_facade_lists_its_top_level_functions_local_delegated_properties() {
    let src = format!(
        "{CELL}\n\
         fun first(): Int {{\n\
         \x20   val a by Cell(1)\n\
         \x20   return a\n\
         }}\n\
         \n\
         fun second(): Int {{\n\
         \x20   var b by Cell(2)\n\
         \x20   b = 3\n\
         \x20   return b\n\
         }}\n"
    );
    common::assert_classes_identical_to_kotlinc("FacadeAccessors", &src, &["FacadeAccessorsKt"]);
}

#[test]
fn member_extension_delegate_keeps_the_selected_dispatch_receiver() {
    let src = "import kotlin.reflect.KProperty\n\
         class Token(val text: String)\n\
         class Scope {\n\
         \x20   operator fun Token.getValue(owner: Any?, property: KProperty<*>): String = text\n\
         \x20   fun read(): String { val value by Token(\"OK\"); return value }\n\
         }\n";
    common::assert_classes_identical_to_kotlinc(
        "MemberExtensionDelegate",
        src,
        &["Token", "Scope"],
    );
}

#[test]
fn generic_and_plain_locals_share_one_complete_metadata_container() {
    let src = "import kotlin.reflect.KProperty\n\
         class Box<T>(val value: T) {\n\
         \x20   operator fun getValue(owner: Any?, property: KProperty<*>): T = value\n\
         }\n\
         class Mixed<T>(val input: T) {\n\
         \x20   fun read(): String {\n\
         \x20       val generic: T by Box(input)\n\
         \x20       val plain: String by Box(\"OK\")\n\
         \x20       return plain\n\
         \x20   }\n\
         }\n";
    // `Mixed` owns both local records. `Box` is only their ordinary delegate class and has no local
    // delegated-property metadata of its own, so its independent byte parity cannot mask this gate.
    common::assert_classes_identical_to_kotlinc("MixedLocalMetadata", src, &["Mixed"]);
}

#[test]
fn provider_and_inline_accessor_property_reference_uses_do_not_alias() {
    let src = "class Provider<T>(val initial: T) {
            inline operator fun provideDelegate(owner: Any?, property: Any) = Cell(initial)
        }
        class Cell<T>(var stored: T) {
            inline operator fun getValue(owner: Any?, property: Any): T = stored
            inline operator fun setValue(owner: Any?, property: Any, value: T) {
                stored = value
            }
        }
        fun box(): String {
            val fixed by Provider(\"O\")
            var changed by Provider(\"Fail\")
            changed = \"K\"
            return fixed + changed
        }
    ";
    assert_eq!(
        common::expect_box_run_with_stdlib(src, "IndependentLocalDelegateReferences"),
        "OK"
    );
    common::assert_classes_identical_to_kotlinc(
        "IndependentLocalDelegateReferences",
        src,
        &["IndependentLocalDelegateReferencesKt"],
    );
}

#[test]
fn retained_inline_local_delegate_materializes_its_sibling_inline_convention() {
    let sources = [
        (
            "Library.kt",
            "import kotlin.reflect.KProperty\n\
             inline operator fun String.getValue(owner: Any?, property: KProperty<*>): String =\n\
             \x20   property.name + this\n\
             object C {\n\
             \x20   inline fun inlineFun() = {\n\
             \x20       val O by \"K\"\n\
             \x20       O\n\
             \x20   }.let { it() }\n\
             }",
        ),
        (
            "Main.kt",
            "object ForceOutOfOrder {\n\
             \x20   fun callInline() = C.inlineFun()\n\
             }\n\
             fun box(): String = ForceOutOfOrder.callInline()",
        ),
    ];

    common::expect_box_ok_files_with_stdlib(
        &sources,
        "retained inline local delegate convention dependency",
    );
    let pair = common::ModuleClassPair::compile(&sources, "ForceOutOfOrder");
    let body = krusty::jvm::classreader::read_method_code(
        &pair.krusty,
        "callInline",
        "()Ljava/lang/String;",
    )
    .expect("consumer inline call body");
    let calls = krusty::jvm::inline::disassemble(&body.code)
        .expect("consumer inline call instructions")
        .iter()
        .filter_map(|instruction| krusty::jvm::inline::invoked_method(instruction, &body.source_cp))
        .map(|(owner, name, descriptor, _)| {
            (owner.to_owned(), name.to_owned(), descriptor.to_owned())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        calls,
        vec![(
            "kotlin/jvm/functions/Function0".to_string(),
            "invoke".to_string(),
            "()Ljava/lang/Object;".to_string(),
        )],
        "the retained template must contain the materialized lambda invocation, with neither the original inline call nor its local-delegate convention call left over",
    );
}
