//! Writing a class copied from a compiled one, as kotlinc's `AnonymousObjectTransformer` does
//! through ASM: each declaration is interned when it is visited, in the order the transformer
//! visits it, rather than in the order kotlinc's `ClassCodegen` writes a class of its own.

use super::constant_pool_queries::PoolLookup;
use super::method_rewrite::MethodIdentity;
use super::{u2, ClassWriter, FieldInfo, MethodInfo};
use crate::jvm::class_node::{Annotation, ClassMethod, ElementValue, FieldNode};
use crate::jvm::method_node::{AssembleError, Constant};
use crate::jvm::source_map::SourceMap;

/// Why a declaration could not be copied as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CopyError {
    /// The declaration carries something this writer does not write.
    Unsupported(&'static str),
    /// The body does not lay out.
    Assemble(AssembleError),
}

/// `kotlin.jvm.internal.SourceDebugExtension`, the copy of the source map kotlinc also writes as an
/// annotation.
const SOURCE_DEBUG_EXTENSION_DESC: &str = "Lkotlin/jvm/internal/SourceDebugExtension;";

impl ClassWriter {
    /// Intern a `CONSTANT_NameAndType` (and its two names) where a visit introduces it.
    pub(crate) fn seed_name_and_type(&mut self, name: &str, desc: &str) {
        self.cp.name_and_type(name, desc);
    }

    /// `ClassVisitor.visitField`: the field's name, descriptor, signature and constant value intern
    /// here, then its annotations.
    pub(crate) fn add_copied_field(&mut self, field: &FieldNode) -> Result<(), CopyError> {
        if field.deprecated {
            return Err(CopyError::Unsupported("a deprecated field"));
        }
        let name = self.cp.utf8(&field.name);
        let desc = self.cp.utf8(&field.desc);
        let signature = field
            .signature
            .as_deref()
            .map(|signature| self.cp.utf8(signature));
        let const_value = field.value.as_ref().map(|value| self.constant(value));
        let visible_anns = self.copied_annotations(&field.visible_annotations);
        let invisible_anns = self.copied_annotations(&field.invisible_annotations);
        self.fields.push(FieldInfo {
            access: field.access,
            name,
            desc,
            signature,
            const_value,
            visible_anns,
            invisible_anns,
            pending_visible: Vec::new(),
            pending_invisible: Vec::new(),
        });
        Ok(())
    }

    /// `ClassVisitor.visitAnnotation` on the class.
    pub(crate) fn add_copied_class_annotation(&mut self, annotation: &Annotation, visible: bool) {
        let encoded = self.copied_annotation(annotation);
        if visible {
            self.runtime_annotations.push(encoded);
        } else {
            self.invisible_annotations.push(encoded);
        }
    }

    /// `ClassVisitor.visitMethod` followed by the method's body, as `MethodNode.accept` visits it:
    /// the declaration, its annotations, then the code and its local variables. The body then
    /// passes through kotlinc's method optimizations, which every class writer applies.
    pub(crate) fn add_copied_method(&mut self, method: &ClassMethod) -> Result<(), CopyError> {
        if !method.exceptions.is_empty() {
            return Err(CopyError::Unsupported("an Exceptions attribute"));
        }
        if method.annotation_default.is_some() {
            return Err(CopyError::Unsupported("an AnnotationDefault attribute"));
        }
        let name = self.cp.utf8(&method.name);
        let desc = self.cp.utf8(&method.desc);
        let signature = method
            .signature
            .as_deref()
            .map(|signature| self.cp.utf8(signature));
        let visible_anns = self.copied_annotations(&method.visible_annotations);
        let invisible_anns = self.copied_annotations(&method.invisible_annotations);
        let visible_param_anns =
            self.copied_parameter_annotations(&method.visible_parameter_annotations);
        let user_invisible_param_anns =
            self.copied_parameter_annotations(&method.invisible_parameter_annotations);
        if method.deprecated {
            self.deprecated_methods.insert((name, desc));
        }
        let mut info = MethodInfo {
            access: method.access,
            name,
            desc,
            max_stack: 0,
            max_locals: 0,
            code: None,
            implicit_void_return_pc: None,
            rewrite_source: None,
            exceptions: Vec::new(),
            stackmap: None,
            signature,
            lnt: Vec::new(),
            lvt: Vec::new(),
            visible_anns,
            invisible_anns,
            param_anns: Vec::new(),
            visible_param_anns,
            user_invisible_param_anns,
            annotation_default: false,
            method_parameters: Vec::new(),
        };
        let Some(node) = &method.code else {
            self.methods.push(info);
            return Ok(());
        };
        let assembled = node.assemble(self).map_err(CopyError::Assemble)?;
        info.lvt = assembled
            .local_variables
            .iter()
            .map(|local| {
                (
                    self.cp.utf8(&local.name),
                    self.cp.utf8(&local.desc),
                    local.slot,
                    Some(local.start_pc),
                    Some(local.length),
                )
            })
            .collect();
        info.max_stack = assembled.max_stack;
        info.max_locals = assembled.max_locals;
        info.code = Some(assembled.code);
        info.exceptions = assembled.exception_table;
        info.lnt = assembled.line_numbers;
        self.methods.push(info);
        let index = self.methods.len() - 1;
        let identity = MethodIdentity {
            access: method.access,
            name: &method.name,
            desc: &method.desc,
        };
        let mut pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
        let optimized = self.optimized(
            &self.methods[index],
            identity,
            node.clone(),
            None,
            &mut pool,
        );
        if let Some(optimized) = optimized {
            self.methods[index].take_rewritten(optimized);
        }
        Ok(())
    }

    /// Give the class `map` as its source map, which the class then writes whatever it holds.
    pub(crate) fn set_source_map(&mut self, map: SourceMap) {
        self.source_map = map;
    }

    /// `AbstractClassBuilder.done(generateSmapCopyToAnnotation)`: the source file's name, then the
    /// source map's annotation copy, intern where the class is finished, ahead of every attribute
    /// name.
    pub(crate) fn intern_source_and_map(&mut self) {
        if let Some(source_file) = self.source_file.clone() {
            self.cp.utf8(&source_file);
        }
        if let Some(map) = self.source_map.render() {
            self.cp.utf8(SOURCE_DEBUG_EXTENSION_DESC);
            self.cp.utf8("value");
            self.cp.utf8(&map);
        }
    }

    fn copied_parameter_annotations(
        &mut self,
        parameters: &Option<Vec<Vec<Annotation>>>,
    ) -> Vec<Vec<Vec<u8>>> {
        parameters
            .iter()
            .flatten()
            .map(|annotations| self.copied_annotations(annotations))
            .collect()
    }

    fn copied_annotations(&mut self, annotations: &[Annotation]) -> Vec<Vec<u8>> {
        annotations
            .iter()
            .map(|annotation| self.copied_annotation(annotation))
            .collect()
    }

    /// An `annotation` structure, its constants interned in `AnnotationWriter`'s order.
    fn copied_annotation(&mut self, annotation: &Annotation) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_copied_annotation(&mut out, annotation);
        out
    }

    fn encode_copied_annotation(&mut self, out: &mut Vec<u8>, annotation: &Annotation) {
        let desc = self.cp.utf8(&annotation.desc);
        u2(out, desc);
        u2(out, annotation.values.len() as u16);
        for (name, value) in &annotation.values {
            let name = self.cp.utf8(name);
            u2(out, name);
            self.encode_copied_value(out, value);
        }
    }

    fn encode_copied_value(&mut self, out: &mut Vec<u8>, value: &ElementValue) {
        match value {
            ElementValue::Int(tag, value) => {
                out.push(*tag);
                let index = self.cp.integer(*value);
                u2(out, index);
            }
            ElementValue::Long(value) => {
                out.push(b'J');
                let index = self.cp.long(*value);
                u2(out, index);
            }
            ElementValue::Float(bits) => {
                out.push(b'F');
                let index = self.cp.float(f32::from_bits(*bits));
                u2(out, index);
            }
            ElementValue::Double(bits) => {
                out.push(b'D');
                let index = self.cp.double(f64::from_bits(*bits));
                u2(out, index);
            }
            ElementValue::String(text) => self.ev_str_kt(out, text),
            ElementValue::Enum(desc, constant) => {
                out.push(b'e');
                let desc = self.cp.utf8(desc);
                u2(out, desc);
                let constant = self.cp.utf8(constant);
                u2(out, constant);
            }
            ElementValue::Class(desc) => {
                out.push(b'c');
                let index = self.cp.utf8(desc);
                u2(out, index);
            }
            ElementValue::Annotation(annotation) => {
                out.push(b'@');
                self.encode_copied_annotation(out, annotation);
            }
            ElementValue::Array(values) => {
                out.push(b'[');
                u2(out, values.len() as u16);
                for value in values {
                    self.encode_copied_value(out, value);
                }
            }
        }
    }

    fn constant(&mut self, value: &Constant) -> u16 {
        crate::jvm::method_node::ConstantSink::constant(self, value)
    }
}
