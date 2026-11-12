use super::{compile_program, compile_program_with_provider, Entry, WasmBackend, WasmTarget};
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

/// An abstract class implementing an interface with a default, as the first file declares it.
const BASE: &str = "interface Named {\n\
    \x20   fun name(): String\n\
    \x20   fun greet(): String = \"hi \" + name()\n\
    }\n\
    abstract class Resource(val label: String) : Named {\n\
    \x20   var opened = 0\n\
    \x20   abstract fun open(): String\n\
    \x20   open fun suffix(): String = \"!\"\n\
    \x20   override fun name(): String = label\n\
    }\n";

/// A class of a second file extending it: a superclass constructor call with an argument, an
/// override of an abstract member, an override calling its `super` body, and a call through the
/// interface whose default reaches an implementation the first file declares.
const SUBCLASS: &str =
    "class FileResource(label: String, private val content: String) : Resource(label) {\n\
    \x20   override fun open(): String {\n\
    \x20       opened++\n\
    \x20       return content\n\
    \x20   }\n\
    \x20   override fun suffix(): String = super.suffix() + \"?\"\n\
    \x20   fun read(): String = open() + suffix()\n\
    }\n\
    fun box(): String {\n\
    \x20   val resource = FileResource(\"file\", \"K\")\n\
    \x20   if (resource.read() != \"K!?\") return \"fail read\"\n\
    \x20   val named: Named = resource\n\
    \x20   return if (named.greet() == \"hi file\") \"OK\" else \"fail greet\"\n\
    }\n";

#[test]
fn a_class_extends_one_an_earlier_file_laid_out() {
    for target in WasmTarget::ALL {
        let (outputs, diagnostics) = compile_program(target, Entry::Box, &[BASE, SUBCLASS]);
        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(outputs.len(), 2);
    }
}

#[test]
fn a_class_whose_superclass_a_later_file_declares_declines_by_name() {
    // The superclass's layout is the later file's, which is not lowered yet when the subclass is.
    for target in WasmTarget::ALL {
        let (outputs, diagnostics) = compile_program(target, Entry::Box, &[SUBCLASS, BASE]);
        assert_eq!(
            diagnostics,
            vec![
                "krusty: the wasm backend does not support a superclass declared outside this \
                 file (`FileResource` extends `Resource`) yet"
                    .to_string()
            ]
        );
        assert_eq!(outputs, Vec::new());
    }
}

/// A runtime exception subclass, as the first file declares it: its struct extends the runtime
/// exception struct and carries the message and cause.
const EXCEPTION_BASE: &str = "open class AppFailure(message: String?) : IllegalStateException(message) {\n\
    \x20   open fun code(): Int = 1\n\
    }\n\
    open class CausedFailure(message: String, cause: Throwable) : RuntimeException(message, cause)\n";

/// Subclasses of them in a second file: a primary and a secondary constructor delegating to the
/// recorded constructors, an override calling its `super` body, and catches by the subclass and
/// by a runtime parent.
const EXCEPTION_SUBCLASSES: &str =
    "class MissingFailure(val what: String) : AppFailure(\"missing \" + what) {\n\
    \x20   override fun code(): Int = super.code() + 1\n\
    }\n\
    class WrappedFailure : CausedFailure {\n\
    \x20   constructor(cause: Throwable) : super(\"wrapped\", cause)\n\
    }\n\
    fun box(): String {\n\
    \x20   try {\n\
    \x20       throw MissingFailure(\"key\")\n\
    \x20   } catch (e: MissingFailure) {\n\
    \x20       if (e.message != \"missing key\" || e.code() != 2) return \"fail missing\"\n\
    \x20   }\n\
    \x20   try {\n\
    \x20       throw WrappedFailure(IllegalArgumentException(\"inner\"))\n\
    \x20   } catch (e: RuntimeException) {\n\
    \x20       if (e.message != \"wrapped\") return \"fail wrapped\"\n\
    \x20   }\n\
    \x20   return \"OK\"\n\
    }\n";

#[test]
fn a_class_extends_a_runtime_exception_subclass_an_earlier_file_laid_out() {
    for target in WasmTarget::ALL {
        let path = krusty::toolchain::kotlin_stdlib_klib(
            krusty::backend::Backend::compilation_target(&WasmBackend::new(target)),
        )
        .expect("the exact target stdlib must be provisioned");
        let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
            path,
            KlibStdlibSemantics::Wasm,
        )])
        .unwrap();
        let (outputs, diagnostics) = compile_program_with_provider(
            target,
            Entry::Box,
            &[EXCEPTION_BASE, EXCEPTION_SUBCLASSES],
            Box::new(libraries),
        );
        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(outputs.len(), 2);
    }
}
