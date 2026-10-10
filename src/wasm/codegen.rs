//! krusty's WebAssembly code generator: checked common IR to one WasmGC module.
//!
//! Each file is lowered into the module as it arrives; the module is encoded when the backend is
//! finalized, because wasm is linked as a whole (see the module docs in `super`). Anything the
//! generator has not been taught declines the compilation with a diagnostic naming the construct,
//! and a declined compilation emits nothing.

mod lower;

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
        let selected = self.entry.selected(&file.ir);
        if selected.is_some_and(|(_, parameters)| {
            parameters == crate::backend::MainEntryParameters::Arguments
        }) {
            decline(state, "a `main` that takes its arguments".to_string());
            return Vec::new();
        }
        crate::backend::counted_loops::realize(&mut file.ir, COUNTED_LOOPS);
        if crate::backend::local_properties::realize(&mut file.ir).is_err() {
            decline(state, "a local delegated property".to_string());
            return Vec::new();
        }
        let started = state
            .started
            .get_or_insert_with(|| Started::new(self.target));
        let lowered = match lower::lower_file(&file.ir, &mut started.module, &mut started.runtime) {
            Ok(lowered) => lowered,
            Err(construct) => {
                decline(state, construct);
                return Vec::new();
            }
        };
        state.initializers.extend(lowered.initializer);
        if let Some((function, _)) = selected {
            let stem = file.stems[file.source.raw() as usize].clone();
            match &state.entry {
                Some((_, first)) => diags.error(
                    crate::diag::Span::new(0, 0),
                    format!(
                        "krusty: the program declares {} in both {first} and {stem}",
                        self.entry.declaration()
                    ),
                ),
                None => state.entry = Some((lowered.functions[function as usize], stem)),
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
    fn an_unsupported_construct_declines_the_whole_compilation_by_name() {
        let (outputs, diagnostics) = compile(
            WasmTarget::Js,
            "class Box(val value: String)\nfun box(): String = Box(\"OK\").value",
        );
        assert_eq!(outputs, Vec::<Artifact>::new());
        assert_eq!(
            diagnostics,
            ["krusty: the wasm backend does not support a class yet"]
        );
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
    fn a_main_program_starts_in_main() {
        let (outputs, diagnostics) =
            compile_program(WasmTarget::Wasi, Entry::Main, &["fun main() {}"]);
        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(outputs.len(), 2);
    }
}
