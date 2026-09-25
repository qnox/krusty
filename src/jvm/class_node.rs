//! A whole class file as kotlinc's ASM `ClassReader` hands it to a `ClassVisitor`: the class's
//! declarations, attributes and annotations, and each method's body as a [`MethodNode`].
//!
//! kotlinc reads a compiled class this way when it copies one: an anonymous object or lambda in an
//! inline function's body is regenerated for each call site under a new name
//! (`AnonymousObjectTransformer`). Every operand is symbolic, so the copy can be written into a
//! new constant pool. A class carrying an attribute the copy would not reproduce is refused rather
//! than copied without it; frames are not kept (`ClassReader.SKIP_FRAMES`), the writer computes
//! them.

mod annotation;
mod read;

use crate::jvm::method_node::{Constant, MethodNode};

pub(crate) use annotation::{Annotation, ElementValue};
pub(crate) use read::ClassReadError;

/// One class file.
#[derive(Clone, Debug)]
pub(crate) struct ClassNode {
    pub major: u16,
    pub access: u16,
    pub name: String,
    pub signature: Option<String>,
    pub super_name: Option<String>,
    pub interfaces: Vec<String>,
    /// `SourceFile`.
    pub source_file: Option<String>,
    /// `SourceDebugExtension`, the class's SMAP.
    pub source_debug: Option<String>,
    /// `EnclosingMethod`.
    pub outer_class: Option<OuterClass>,
    /// `InnerClasses`, in table order.
    pub inner_classes: Vec<InnerClass>,
    pub visible_annotations: Vec<Annotation>,
    pub invisible_annotations: Vec<Annotation>,
    pub deprecated: bool,
    pub fields: Vec<FieldNode>,
    pub methods: Vec<ClassMethod>,
}

/// `EnclosingMethod`: the class, and the method when there is one, a local class is declared in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OuterClass {
    pub owner: String,
    /// The enclosing method's name and descriptor.
    pub method: Option<(String, String)>,
}

/// One `InnerClasses` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InnerClass {
    pub name: String,
    pub outer_name: Option<String>,
    pub inner_name: Option<String>,
    pub access: u16,
}

/// One field.
#[derive(Clone, Debug)]
pub(crate) struct FieldNode {
    pub access: u16,
    pub name: String,
    pub desc: String,
    pub signature: Option<String>,
    /// `ConstantValue`.
    pub value: Option<Constant>,
    pub visible_annotations: Vec<Annotation>,
    pub invisible_annotations: Vec<Annotation>,
    pub deprecated: bool,
}

/// One method: its declaration and, unless it is abstract or native, its body.
#[derive(Clone, Debug)]
pub(crate) struct ClassMethod {
    pub access: u16,
    pub name: String,
    pub desc: String,
    pub signature: Option<String>,
    /// `Exceptions`.
    pub exceptions: Vec<String>,
    pub visible_annotations: Vec<Annotation>,
    pub invisible_annotations: Vec<Annotation>,
    /// `RuntimeVisibleParameterAnnotations`, one list per parameter the attribute counts.
    pub visible_parameter_annotations: Option<Vec<Vec<Annotation>>>,
    /// `RuntimeInvisibleParameterAnnotations`.
    pub invisible_parameter_annotations: Option<Vec<Vec<Annotation>>>,
    /// `AnnotationDefault`.
    pub annotation_default: Option<ElementValue>,
    pub deprecated: bool,
    pub code: Option<MethodNode>,
}

impl ClassNode {
    /// Read the class file `bytes`.
    pub(crate) fn read(bytes: &[u8]) -> Result<ClassNode, ClassReadError> {
        read::read_class(bytes)
    }
}
