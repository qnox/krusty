//! Collects the module's [`ModuleIr`] from Pass 2's checked bodies: every top-level function of
//! every Kotlin source, numbered by file and position as the plugin IR names them.

use crate::compiler::{CheckedBodyObserver, ObservedBody, ObservedModule};
use crate::diag::Span;
use crate::fir::{DeclarationId, DeclarationKind, SourceFileId};
use crate::fx_hash::HashMap;
use krusty_plugin_ir::{FileIr, FunctionIr, ModuleIr};

use super::call_spans::CallSpans;
use super::function::encode_function;
use super::offsets::Utf16Offsets;
use super::types::package_name;
use super::Refusal;

pub(super) struct ModuleIrEncoder<'p> {
    /// The path of each source input, in input order.
    paths: &'p [String],
    files: Vec<FileState>,
    /// The plugin IR's numbering of each top-level function: its file, then its position there.
    pub(super) positions: HashMap<DeclarationId, (u32, u32)>,
    refusal: Option<Refusal>,
}

struct FileState {
    source: SourceFileId,
    ir: FileIr,
    offsets: Utf16Offsets,
    /// Each function's name, and its IR once its body is checked, by position.
    functions: Vec<(String, Option<FunctionIr>)>,
}

impl<'p> ModuleIrEncoder<'p> {
    pub(super) fn new(paths: &'p [String]) -> Self {
        Self {
            paths,
            files: Vec::new(),
            positions: HashMap::default(),
            refusal: None,
        }
    }

    fn refuse(&mut self, refusal: Refusal) {
        self.refusal.get_or_insert(refusal);
    }

    /// The module's IR, or why the plugin IR cannot spell it: the first refusal in Pass 2's order,
    /// or a function whose body never reached Pass 2 as an ordinary body.
    pub(super) fn finish(self) -> Result<ModuleIr, Refusal> {
        if let Some(refusal) = self.refusal {
            return Err(refusal);
        }
        let files = self
            .files
            .into_iter()
            .map(|file| {
                let functions = file
                    .functions
                    .into_iter()
                    .map(|(name, function)| {
                        function.ok_or_else(|| Refusal {
                            source: file.source,
                            span: Span::new(0, 0),
                            what: format!(
                                "the function `{name}`, whose body is not checked as an ordinary body"
                            ),
                        })
                    })
                    .collect::<Result<_, _>>()?;
                Ok(FileIr {
                    functions,
                    ..file.ir
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(ModuleIr { files })
    }
}

impl CheckedBodyObserver for ModuleIrEncoder<'_> {
    fn begin_module(&mut self, module: ObservedModule<'_>) {
        for source in module.sources {
            let file = self.files.len() as u32;
            let mut functions = Vec::new();
            for &declaration in module.index.source_inventory(source.id) {
                let Some(anchor) = module.index.declaration_anchor(declaration) else {
                    continue;
                };
                let name = module.index.declaration_name(declaration).unwrap_or("");
                if anchor.kind == DeclarationKind::Function && anchor.owner.is_none() {
                    self.positions
                        .insert(declaration, (file, functions.len() as u32));
                    functions.push((name.to_string(), None));
                } else if anchor.owner.is_none() {
                    self.refuse(Refusal {
                        source: source.id,
                        span: Span::new(0, 0),
                        what: format!("the {:?} declaration `{name}`", anchor.kind),
                    });
                }
            }
            let package = module
                .source_map
                .get(source.id)
                .map(|file| package_name(file.package))
                .unwrap_or_default();
            self.files.push(FileState {
                source: source.id,
                ir: FileIr {
                    path: self.paths[source.id.raw() as usize].clone(),
                    package,
                    text: source.text.to_string(),
                    functions: Vec::new(),
                },
                offsets: Utf16Offsets::new(source.text),
                functions,
            });
        }
    }

    fn observe(&mut self, observed: ObservedBody<'_>) {
        if self.refusal.is_some() {
            return;
        }
        let declaration = DeclarationId::from_raw(observed.owner.raw());
        let Some(&(file, position)) = self.positions.get(&declaration) else {
            let name = observed.index.declaration_name(declaration).unwrap_or("");
            self.refuse(Refusal {
                source: observed.source,
                span: Span::new(0, 0),
                what: format!("the body of `{name}`"),
            });
            return;
        };
        if observed.body.is_default_fragment() {
            let name = observed.index.declaration_name(declaration).unwrap_or("");
            self.refuse(Refusal {
                source: observed.source,
                span: Span::new(0, 0),
                what: format!("a default value of a parameter of `{name}`"),
            });
            return;
        }
        let state = &self.files[file as usize];
        // Each body's syntax is parsed for it alone.
        let calls = CallSpans::new(observed.file);
        match encode_function(
            &observed,
            declaration,
            &state.offsets,
            &calls,
            &self.positions,
        ) {
            Ok(function) => {
                self.files[file as usize].functions[position as usize].1 = Some(function)
            }
            Err(refusal) => self.refuse(refusal),
        }
    }
}
