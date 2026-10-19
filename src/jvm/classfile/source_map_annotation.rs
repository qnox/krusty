//! Source-map initialization, the per-method maps, and the
//! `@kotlin.jvm.internal.SourceDebugExtension` copy.
//!
//! kotlinc numbers the inlined lines of each method against a map of that method's own
//! (`ClassCodegen.generateMethodNode`), and its coroutine transformer records those numbers in a
//! continuation's `@DebugMetadata`. Only when the method is written does it copy them into the
//! class map, method by method, in the order the class lists them.
//!
//! kotlinc publishes the source map a second time as a BINARY-retained annotation, which is how a
//! Kotlin consumer reads it back without parsing the class file's own attribute. Its `value` is a
//! string array: one element, or, for a map longer than one `CONSTANT_Utf8` entry holds, the parts
//! kotlinc's `splitStringConstant` cuts it into.

use super::{u2, ClassWriter, ConstPool};
use crate::jvm::string_constant::text_entry_parts;

const SOURCE_DEBUG_EXTENSION_DESC: &str = "Lkotlin/jvm/internal/SourceDebugExtension;";

use crate::jvm::source_map::SourceMap;

impl ClassWriter {
    /// The map an inlined line of the method being emitted is numbered against, started from the
    /// class's own source file the first time an inline body is spliced into it. Splicing a
    /// dependency's `inline fun` puts that dependency's source into this class, and the map is what
    /// tells a debugger which file an inlined line belongs to.
    ///
    /// A class regenerated from another class's declaration maps straight into its own map, whose
    /// owning file names that declaration class.
    ///
    /// `lines` is the highest line the class's own code can claim. `None` when the class has no
    /// `SourceFile` to map against, in which case its inlined lines cannot be described at all.
    pub fn source_map_for_inlining(&mut self, lines: u16) -> Option<&mut SourceMap> {
        if self.source_map.is_unstarted() {
            let source_file = self.source_file.clone()?;
            self.source_map = SourceMap::new(&source_file, &self.internal_name, lines);
        }
        if self.source_map.owner_path() != Some(self.internal_name.as_str()) {
            return Some(&mut self.source_map);
        }
        if self.method_source_map.is_unstarted() {
            self.method_source_map = self.source_map.for_method()?;
        }
        Some(&mut self.method_source_map)
    }

    /// The map of the method just emitted, when anything was inlined into it, leaving the next
    /// method to start its own.
    pub(super) fn take_method_source_map(&mut self) -> Option<SourceMap> {
        let map = std::mem::take(&mut self.method_source_map);
        (!map.is_empty()).then_some(map)
    }

    /// The lines of each method that has a map of its own, in the order its table lists them
    /// before the bytecode rewrites run: the order kotlinc's copier reads them in.
    pub(super) fn written_method_lines(&self) -> Vec<Option<Vec<u16>>> {
        self.methods
            .iter()
            .map(|method| {
                method
                    .source_map
                    .as_ref()
                    .map(|_| method.lnt.iter().map(|&(_, line)| line).collect())
            })
            .collect()
    }

    /// Renumber every method's inlined lines into the class map, in the order the class lists the
    /// methods, once their bodies are final (`SourceMapCopyingMethodVisitor`). `written` is what
    /// [`ClassWriter::written_method_lines`] returned before the rewrites.
    pub(super) fn copy_method_source_maps(&mut self, written: &[Option<Vec<u16>>]) {
        for (method, written) in self.methods.iter_mut().zip(written) {
            if let Some(map) = method.source_map.take() {
                let written = written.as_deref().unwrap_or_default();
                self.source_map
                    .copy_method_lines(&map, written, &mut method.lnt);
            }
        }
    }

    /// Start the identity source map kotlinc writes for a class generated inside an `inline`
    /// function (`visitSMAP(…, intoInline = true)`). The map is kept even when the class inlines
    /// nothing else. A map an earlier splice already started is left as it is.
    pub fn start_inline_origin_source_map(&mut self, lines: u16) {
        let path = self.internal_name.clone();
        self.start_inline_origin_source_map_at(lines, &path);
    }

    /// Start the class's identity map over the `lines` its own source file has, before code from
    /// a source file with another line count is emitted into it: the map an inlined line is then
    /// numbered against claims lines past this file's own, whichever file the emission runs for.
    pub fn start_source_map(&mut self, lines: u16) {
        if self.source_map.is_unstarted() {
            let Some(source_file) = self.source_file.clone() else {
                return;
            };
            self.source_map = SourceMap::new(&source_file, &self.internal_name, lines);
        }
    }

    /// Start a regenerated inline class's identity map under its declaration class path. A
    /// call-site copy has a different emitted internal name, but its primary source remains the
    /// declaration lambda class that the inliner regenerated.
    pub fn start_inline_origin_source_map_at(&mut self, lines: u16, path: &str) {
        if self.source_map.is_unstarted() {
            let Some(source_file) = self.source_file.clone() else {
                return;
            };
            self.source_map =
                crate::jvm::source_map::SourceMap::for_regenerated_class(&source_file, path, lines);
        }
    }
}

impl ConstPool {
    /// Intern the annotation's constants in kotlinc's order and return its `annotation` structure.
    pub(super) fn source_map_annotation(&mut self, map: &str) -> Vec<u8> {
        let mut body = Vec::new();
        let annotation = self.utf8(SOURCE_DEBUG_EXTENSION_DESC);
        u2(&mut body, annotation);
        u2(&mut body, 1); // one element pair
        let name = self.utf8("value");
        u2(&mut body, name);
        body.push(b'[');
        let parts = text_entry_parts(map);
        u2(
            &mut body,
            u16::try_from(parts.len()).expect("a source map fits one class file"),
        );
        for part in &parts {
            body.push(b's');
            let value = self.utf8_kt(part);
            u2(&mut body, value);
        }
        body
    }
}
