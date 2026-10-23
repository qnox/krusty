//! krusty's WebAssembly code generator: checked common IR to one WasmGC module.
//!
//! Each file is lowered into the module as it arrives; the module is encoded when the backend is
//! finalized, because wasm is linked as a whole (see the module docs in `super`). Anything the
//! generator has not been taught declines the compilation with a diagnostic naming the construct,
//! and a declined compilation emits nothing.

mod callable_registry;
mod classes;
mod lower;
mod property_registry;

use crate::backend::{Artifact, Backend, CheckedIrFile, Entry, BOX_RESULT_FRAME};
use crate::diag::DiagSink;

use super::encode::{Code, Function, Module};
use super::host::Host;
use super::runtime::Runtime;
use super::target::WasmTarget;

/// What every decline begins with; the construct follows.
pub const DECLINE_PREFIX: &str = "krusty: the wasm backend does not support ";

pub struct WasmBackend {
    target: WasmTarget,
    entry: Entry,
}

impl WasmBackend {
    pub fn new(target: WasmTarget) -> Self {
        Self {
            target,
            entry: Entry::Main,
        }
    }

    /// Start the program in `entry` instead of `main`.
    pub fn with_entry(mut self, entry: Entry) -> Self {
        self.entry = entry;
        self
    }
}

/// The module being assembled, across the files of one compilation.
#[derive(Default)]
pub struct WasmModule {
    started: Option<Started>,
    /// A file was declined, so the module is incomplete and is never encoded.
    declined: bool,
    /// The function the program starts in, and the file that declares it, once one has.
    entry: Option<(u32, String)>,
    /// Each file's top-level property initializer, in file order. They all run before the entry,
    /// as the native target runs them at program start: a program's first touch of a file is
    /// through its entry or a call that follows it, so starting with every initializer is the same
    /// order the JVM's lazy facade initialization observes for a program declared in one file.
    initializers: Vec<u32>,
}

struct Started {
    module: Module,
    host: Host,
    runtime: Runtime,
    callables: callable_registry::CallableRegistry,
    properties: property_registry::PropertyRegistry,
}

impl Started {
    fn new(target: WasmTarget) -> Self {
        let mut module = Module::default();
        // The object model's types come first, so their indices are the ones `lower` names.
        Runtime::declare_types(&mut module);
        let mut host = Host::import(&mut module, target);
        host.declare(&mut module);
        let runtime = Runtime::start(&mut module, &host);
        Self {
            module,
            host,
            runtime,
            callables: Default::default(),
            properties: Default::default(),
        }
    }
}

impl Backend for WasmBackend {
    type State = WasmModule;

    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget {
        match self.target {
            WasmTarget::Js => crate::compilation_target::CompilationTarget::WasmJs,
            WasmTarget::Wasi => crate::compilation_target::CompilationTarget::WasmWasi,
        }
    }

    fn emits_dependency_support(&self) -> bool {
        true
    }

    fn lower_ir_file(
        &self,
        mut file: CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        if state.declined {
            return Vec::new();
        }
        let mut decline = |state: &mut WasmModule, construct: String| {
            state.declined = true;
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("{DECLINE_PREFIX}{construct} yet"),
            );
        };
        let selected = match file.kind {
            crate::backend::CheckedIrUnitKind::Source => self.entry.selected(&file.ir),
            crate::backend::CheckedIrUnitKind::DependencySupport => None,
        };
        if selected.is_some_and(|(_, parameters)| {
            parameters == crate::backend::MainEntryParameters::Arguments
        }) {
            decline(state, "a `main` that takes its arguments".to_string());
            return Vec::new();
        }
        crate::backend::counted_loops::realize(&mut file.ir, COUNTED_LOOPS);
        let Ok(properties) = crate::backend::local_properties::realize(&mut file.ir) else {
            decline(state, "a local delegated property".to_string());
            return Vec::new();
        };
        let started = state
            .started
            .get_or_insert_with(|| Started::new(self.target));
        if file.kind == crate::backend::CheckedIrUnitKind::Source {
            // A support unit has no source `CallableId`s. It is emitted immediately before the
            // source file that selected it, so rotating the source registry here would discard
            // cross-file reservations from the current source module.
            started.callables.enter_source_module(file.module_name);
            started.properties.enter_source_module(file.module_name);
        }
        let lowered = match lower::lower_file(
            &file.ir,
            &properties,
            &file.callables,
            &file.dependency_exports,
            &mut started.module,
            &mut started.runtime,
            &mut started.callables,
            &mut started.properties,
        ) {
            Ok(lowered) => lowered,
            Err(construct) => {
                decline(state, construct);
                return Vec::new();
            }
        };
        state.initializers.extend(lowered.initializer);
        if let Some((function, _)) = selected {
            let entry = match lowered.functions.get(function as usize).copied() {
                Some(Some(entry)) => entry,
                Some(None) => {
                    decline(state, "an inline-only entry function".to_string());
                    return Vec::new();
                }
                None => {
                    decline(state, "an invalid entry function identity".to_string());
                    return Vec::new();
                }
            };
            let stem = file.stems[file.source.raw() as usize].clone();
            match &state.entry {
                Some((_, first)) => diags.error(
                    crate::diag::Span::new(0, 0),
                    format!(
                        "krusty: the program declares {} in both {first} and {stem}",
                        self.entry.declaration()
                    ),
                ),
                None => state.entry = Some((entry, stem)),
            }
        }
        Vec::new()
    }

    fn check_module(&self, state: &Self::State, diags: &mut DiagSink) {
        if state.started.is_some() && !state.declined && state.entry.is_none() {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!(
                    "krusty: the program declares no {} to start in",
                    self.entry.declaration()
                ),
            );
        }
    }

    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact> {
        let (Some(mut started), false, Some((entry, _))) =
            (state.started, state.declined, state.entry)
        else {
            return Vec::new();
        };
        let start = started.module.declare();
        let ty = started.module.func_type(Vec::new(), Vec::new());
        let mut code = Code::default();
        for initializer in &state.initializers {
            code.call(*initializer);
        }
        match self.entry {
            Entry::Main => {
                code.call(entry);
            }
            Entry::Box => {
                let frame = BOX_RESULT_FRAME.encode_utf16().collect::<Vec<_>>();
                started
                    .runtime
                    .string_constant(&mut started.module, &mut code, &frame);
                code.call(started.runtime.write_string)
                    .call(entry)
                    .call(started.runtime.string_or_null)
                    .call(started.runtime.write_string);
            }
        }
        started.module.define(
            start,
            Function {
                ty,
                locals: Vec::new(),
                code,
            },
        );
        started.module.export_function("_start", start);
        vec![
            (format!("{module_name}.wasm"), started.module.encode()),
            (
                format!("{module_name}.mjs"),
                started.host.loader(module_name).into_bytes(),
            ),
        ]
    }
}

/// A counted loop keeps its entry test at the top, as on JavaScript and Native; wasm's `loop`
/// branches back to its start, and the header calls nothing that could be inlined. An `until … step`
/// loop builds its progression, as on JavaScript, until this generator is taught the counted form.
const COUNTED_LOOPS: crate::backend::counted_loops::CountedLoopPolicy =
    crate::backend::counted_loops::CountedLoopPolicy {
        style: crate::backend::counted_loops::CounterLoopStyle::PreTested,
        inlining: crate::backend::counted_loops::HeaderInlining::None,
        until_steps: crate::backend::counted_loops::UntilSteps::Built,
    };

#[cfg(test)]
mod tests {
    use super::{WasmBackend, WasmTarget};
    use crate::backend::{Artifact, Entry};
    use crate::diag::DiagSink;
    use crate::libraries::EmptySymbolSource;
    use crate::source::SourceInput;

    /// Compile `sources`, the file stemmed `Main` first and then `Other1`, `Other2`, …, into a
    /// program starting in `entry`.
    fn compile_program(
        target: WasmTarget,
        entry: Entry,
        sources: &[&str],
    ) -> (Vec<Artifact>, Vec<String>) {
        let stems = (0..sources.len())
            .map(|ordinal| match ordinal {
                0 => "Main".to_string(),
                _ => format!("Other{ordinal}"),
            })
            .collect::<Vec<_>>();
        let inputs = sources
            .iter()
            .zip(&stems)
            .map(|(source, stem)| SourceInput::kotlin(source).with_file_stem(stem))
            .collect::<Vec<_>>();
        let mut diags = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_streaming_with_features(
            &inputs,
            crate::frontend::PlatformProvider::new(
                crate::compiler::Backend::compilation_target(&WasmBackend::new(target)),
                Box::new(EmptySymbolSource),
            ),
            &crate::features::LangFeatures::new(),
            &mut diags,
        );
        let outputs = crate::compiler::emit_analyzed(
            analysis,
            &stems,
            &WasmBackend::new(target).with_entry(entry),
            "main",
            &mut diags,
        );
        let messages = diags.diags.iter().map(|diag| diag.msg.clone()).collect();
        (outputs, messages)
    }

    fn compile(target: WasmTarget, source: &str) -> (Vec<Artifact>, Vec<String>) {
        compile_program(target, Entry::Box, &[source])
    }

    #[test]
    fn a_try_with_finally_still_declines_without_an_artifact() {
        // Wasm backend contract, not a portable semantic claim: finally lowering is a later slice.
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "fun box(): String = try { \"OK\" } finally { val unused = 0 }",
            );
            assert_eq!(outputs, Vec::<Artifact>::new());
            assert_eq!(
                diagnostics,
                ["krusty: the wasm backend does not support a `try` with `finally` yet"]
            );
        }
    }

    #[test]
    fn checked_reference_identity_operators_accept_reference_carriers() {
        // Backend IR contract: source identity reaches PrimitiveBinOp, not structural Equality.
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "class Box(val value: Int)\n\
                 fun box(): String {\n\
                 \x20   val first = Box(7)\n\
                 \x20   val alias = first\n\
                 \x20   val different = Box(7)\n\
                 \x20   if (first !== alias) return \"alias\"\n\
                 \x20   if (first === different) return \"different\"\n\
                 \x20   return \"OK\"\n\
                 }",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn a_program_is_one_module_and_the_loader_that_runs_it() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, "fun box(): String = \"OK\"");
            assert_eq!(diagnostics, Vec::<String>::new());
            let names = outputs
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>();
            assert_eq!(names, ["main.wasm", "main.mjs"]);
            assert_eq!(&outputs[0].1[..8], b"\0asm\x01\0\0\0");
            let loader = String::from_utf8(outputs[1].1.clone()).expect("utf-8 loader");
            assert!(loader.contains("./main.wasm"), "{loader}");
        }
    }

    #[test]
    fn a_cross_file_inline_template_is_not_emitted_as_a_second_declaration() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile_program(
                target,
                Entry::Box,
                &[
                    "inline fun answer(): String = \"OK\"",
                    "fun box(): String = answer()",
                ],
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn an_unsupported_construct_declines_the_whole_compilation_by_name() {
        let (outputs, diagnostics) = compile(
            WasmTarget::Js,
            "enum class Color { RED }\nfun box(): String = \"OK\"",
        );
        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostics,
            ["krusty: the wasm backend does not support an enum class (`Color`) yet"]
        );
    }

    #[test]
    fn a_class_compiles_to_one_module_on_both_targets() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "open class Box(val value: String) { open fun get() = value }\n\
                 class Wrapped(value: String) : Box(value) { override fun get() = value }\n\
                 fun box(): String = (Wrapped(\"OK\") as Box).get()",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn local_class_member_properties_do_not_reserve_module_entries() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "fun box(): String {\n\
                 \x20   class Local(val value: String)\n\
                 \x20   return Local(\"OK\").value\n\
                 }",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn anonymous_object_member_properties_do_not_reserve_module_entries() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "fun box(): String {\n\
                 \x20   val local = object { val value = \"OK\" }\n\
                 \x20   return local.value\n\
                 }",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn nothing_member_property_getter_has_no_wasm_result() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "var flag = true\n\
                 object Test { val magic: Nothing get() = null!! }\n\
                 fun box(): String {\n\
                 \x20   val answer: String\n\
                 \x20   if (flag) answer = \"OK\" else Test.magic\n\
                 \x20   return answer\n\
                 }",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn an_explicitly_returning_getter_does_not_require_a_unit_value() {
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(
                target,
                "var reads = 0\n\
                 val answer: Int get() { reads++; return 42 }\n\
                 fun box(): String = if (answer == 42 && reads == 1) \"OK\" else \"fail\"",
            );
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn a_program_without_its_entry_emits_nothing() {
        for source in [
            "fun notBox(): String = \"OK\"",
            "fun box(x: Int): String = \"OK\"",
            "fun box(): Int = 0",
            "fun outer(): String { fun box(): String = \"OK\"; return box() }",
        ] {
            let (outputs, diagnostics) = compile(WasmTarget::Wasi, source);
            assert_eq!(outputs, Vec::<Artifact>::new(), "{source}");
            assert_eq!(
                diagnostics,
                ["krusty: the program declares no `fun box(): String` to start in"],
                "{source}"
            );
        }
        let (outputs, diagnostics) =
            compile_program(WasmTarget::Js, Entry::Main, &["fun box(): String = \"OK\""]);
        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostics,
            ["krusty: the program declares no `fun main()` to start in"]
        );
    }

    #[test]
    fn a_program_with_two_entries_emits_nothing() {
        let (outputs, diagnostics) = compile_program(
            WasmTarget::Js,
            Entry::Box,
            &[
                "package a\nfun box(): String = \"OK\"",
                "package b\nfun box(): String = \"OK\"",
            ],
        );
        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostics,
            ["krusty: the program declares `fun box(): String` in both Main and Other1"]
        );
    }

    #[test]
    fn a_main_taking_its_arguments_declines_by_name() {
        let (outputs, diagnostics) = compile_program(
            WasmTarget::Wasi,
            Entry::Main,
            &["fun main(args: Array<String>) {}"],
        );
        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostics,
            ["krusty: the wasm backend does not support a `main` that takes its arguments yet"]
        );
    }

    /// Each operator's width comes from the result common lowering recorded for the selected
    /// operator, so every mixed form lowers without the generator deriving a promotion itself.
    #[test]
    fn mixed_numeric_operators_lower_at_their_selected_result() {
        let source = "fun box(): String {\n\
            \x20   val i = 2147483647\n\
            \x20   if (i + 1L != 2147483648L) return \"fail int long\"\n\
            \x20   if ('a' + 2 != 'c') return \"fail char plus\"\n\
            \x20   if ('z' - 'a' != 25) return \"fail char minus\"\n\
            \x20   val b: Byte = 127\n\
            \x20   val s: Short = 2\n\
            \x20   if (b * s != 254) return \"fail narrow\"\n\
            \x20   var c = 'x'\n\
            \x20   c += 2\n\
            \x20   c++\n\
            \x20   if (c != '{') return \"fail char compound\"\n\
            \x20   var l = 1L\n\
            \x20   l += 2\n\
            \x20   l++\n\
            \x20   return if (l == 4L) \"OK\" else \"fail long compound\"\n\
            }\n";
        for target in WasmTarget::ALL {
            let (outputs, diagnostics) = compile(target, source);
            assert_eq!(diagnostics, Vec::<String>::new());
            assert_eq!(outputs.len(), 2);
        }
    }

    #[test]
    fn unsigned_carriers_require_an_explicit_unsigned_operation() {
        assert!(super::lower::carrier(crate::types::Ty::UInt)
            .expect("UInt has a physical carrier")
            .is_some());
        assert_eq!(
            super::lower::ordinary_numeric_carrier(crate::types::Ty::UInt).unwrap_err(),
            "an ordinary operator on `kotlin.UInt`"
        );
        assert_eq!(
            super::lower::ordinary_numeric_carrier(crate::types::Ty::ULong).unwrap_err(),
            "an ordinary operator on `kotlin.ULong`"
        );
    }

    #[test]
    fn a_main_program_starts_in_main() {
        let (outputs, diagnostics) =
            compile_program(WasmTarget::Wasi, Entry::Main, &["fun main() {}"]);
        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(outputs.len(), 2);
    }
}
