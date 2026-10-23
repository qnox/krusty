//! Why two class files with the same name differ: the first kind of difference, in a fixed order,
//! between a class kotlinc wrote and the one krusty wrote for the same compilation.
//!
//! Byte equality is the conformance goal, so a class either matches or has exactly one primary
//! [`DifferenceKind`]: the most structural difference it has. Members come before declarations,
//! declarations before annotations, annotations before code, code before the debug tables that
//! hang off it, and those before the layout the class file alone decides (member order, the
//! constant pool, stack-map frames). A class differing in its instructions and its line numbers is
//! an instruction difference; fixing the instructions usually moves the lines with them.
//!
//! Code is compared by instruction position, not by label: a label is a byte offset, so one more
//! line-number entry renumbers every label after it without changing an instruction.

use std::collections::HashMap;

use crate::jvm::class_node::{Annotation, ClassMethod, ClassNode, FieldNode};
use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

/// The primary way a class differs, most structural first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DifferenceKind {
    /// A field or method only one of the classes declares.
    Members,
    /// The class or a member is declared differently: flags, generic signature, supertypes, inner
    /// class rows, enclosing method, a field's constant, a method's exceptions or parameters.
    Declarations,
    /// The `kotlin.Metadata` annotation.
    KotlinMetadata,
    /// The `kotlin.coroutines.jvm.internal.DebugMetadata` annotation of a coroutine.
    CoroutineDebugMetadata,
    /// Any other annotation, on the class, a member or a parameter.
    Annotations,
    /// A method's instructions or exception handlers.
    Instructions,
    /// A method's `LineNumberTable`.
    LineNumbers,
    /// A method's `LocalVariableTable`.
    LocalVariables,
    /// A method's `max_stack` or `max_locals`.
    StackSizes,
    /// The class's `SourceDebugExtension` (its inlining source map).
    SourceDebug,
    /// The same members, in another order.
    MemberOrder,
    /// Nothing the class declares: the constant pool's order, stack-map frames or attribute order.
    ConstantPool,
    /// One of the classes could not be read structurally.
    Unreadable,
}

impl DifferenceKind {
    /// Every kind, in precedence order.
    pub const ALL: [DifferenceKind; 13] = [
        DifferenceKind::Members,
        DifferenceKind::Declarations,
        DifferenceKind::KotlinMetadata,
        DifferenceKind::CoroutineDebugMetadata,
        DifferenceKind::Annotations,
        DifferenceKind::Instructions,
        DifferenceKind::LineNumbers,
        DifferenceKind::LocalVariables,
        DifferenceKind::StackSizes,
        DifferenceKind::SourceDebug,
        DifferenceKind::MemberOrder,
        DifferenceKind::ConstantPool,
        DifferenceKind::Unreadable,
    ];

    /// The stable identifier a report records.
    pub fn id(self) -> &'static str {
        match self {
            DifferenceKind::Members => "members",
            DifferenceKind::Declarations => "declarations",
            DifferenceKind::KotlinMetadata => "kotlin-metadata",
            DifferenceKind::CoroutineDebugMetadata => "coroutine-debug-metadata",
            DifferenceKind::Annotations => "annotations",
            DifferenceKind::Instructions => "instructions",
            DifferenceKind::LineNumbers => "line-numbers",
            DifferenceKind::LocalVariables => "local-variables",
            DifferenceKind::StackSizes => "stack-sizes",
            DifferenceKind::SourceDebug => "source-debug",
            DifferenceKind::MemberOrder => "member-order",
            DifferenceKind::ConstantPool => "constant-pool",
            DifferenceKind::Unreadable => "unreadable",
        }
    }

    /// The kind [`DifferenceKind::id`] names.
    pub fn from_id(id: &str) -> Option<DifferenceKind> {
        DifferenceKind::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// One line saying what differs.
    pub fn description(self) -> &'static str {
        match self {
            DifferenceKind::Members => "a field or method only one compiler emits",
            DifferenceKind::Declarations => {
                "flags, signatures, supertypes, inner-class rows or other declarations"
            }
            DifferenceKind::KotlinMetadata => "the kotlin.Metadata annotation",
            DifferenceKind::CoroutineDebugMetadata => "coroutine DebugMetadata",
            DifferenceKind::Annotations => "annotations other than Kotlin metadata",
            DifferenceKind::Instructions => "method instructions or exception handlers",
            DifferenceKind::LineNumbers => "line numbers",
            DifferenceKind::LocalVariables => "local variable tables",
            DifferenceKind::StackSizes => "max stack or max locals",
            DifferenceKind::SourceDebug => "the inlining source map (SMAP)",
            DifferenceKind::MemberOrder => "member order",
            DifferenceKind::ConstantPool => "constant pool, frames or attribute order only",
            DifferenceKind::Unreadable => "a class file this comparison cannot read",
        }
    }
}

/// How a class differs: its primary kind and, when one member carries it, that member as
/// `name descriptor` (a method) or `name: descriptor` (a field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassDifference {
    pub kind: DifferenceKind,
    pub member: Option<String>,
}

impl ClassDifference {
    fn new(kind: DifferenceKind, member: Option<String>) -> ClassDifference {
        ClassDifference { kind, member }
    }
}

/// How `candidate` differs from `reference`, or `None` when the two are byte-identical.
pub fn compare_classes(reference: &[u8], candidate: &[u8]) -> Option<ClassDifference> {
    if reference == candidate {
        return None;
    }
    let (Ok(reference), Ok(candidate)) = (ClassNode::read(reference), ClassNode::read(candidate))
    else {
        return Some(ClassDifference::new(DifferenceKind::Unreadable, None));
    };
    Some(
        structural_difference(&reference, &candidate)
            .unwrap_or_else(|| ClassDifference::new(DifferenceKind::ConstantPool, None)),
    )
}

fn structural_difference(reference: &ClassNode, candidate: &ClassNode) -> Option<ClassDifference> {
    let reference_fields = by_key(&reference.fields, field_key);
    let candidate_fields = by_key(&candidate.fields, field_key);
    let reference_methods = by_key(&reference.methods, method_key);
    let candidate_methods = by_key(&candidate.methods, method_key);
    let field_pairs = pairs(&reference.fields, field_key, &candidate_fields);
    let method_pairs = pairs(&reference.methods, method_key, &candidate_methods);

    if let Some(member) = only_in_one(&reference.fields, field_key, &candidate_fields)
        .or_else(|| only_in_one(&candidate.fields, field_key, &reference_fields))
        .or_else(|| only_in_one(&reference.methods, method_key, &candidate_methods))
        .or_else(|| only_in_one(&candidate.methods, method_key, &reference_methods))
    {
        return Some(ClassDifference::new(DifferenceKind::Members, Some(member)));
    }

    if !same_class_declaration(reference, candidate) {
        return Some(ClassDifference::new(DifferenceKind::Declarations, None));
    }
    if let Some((field, _)) = field_pairs
        .iter()
        .find(|(left, right)| !same_field_declaration(left, right))
    {
        return Some(ClassDifference::new(
            DifferenceKind::Declarations,
            Some(field_key(field)),
        ));
    }
    if let Some((method, _)) = method_pairs
        .iter()
        .find(|(left, right)| !same_method_declaration(left, right))
    {
        return Some(ClassDifference::new(
            DifferenceKind::Declarations,
            Some(method_key(method)),
        ));
    }

    for (kind, descriptor) in [
        (DifferenceKind::KotlinMetadata, KOTLIN_METADATA),
        (DifferenceKind::CoroutineDebugMetadata, DEBUG_METADATA),
    ] {
        if annotation(&reference.visible_annotations, descriptor)
            != annotation(&candidate.visible_annotations, descriptor)
        {
            return Some(ClassDifference::new(kind, None));
        }
    }
    if let Some(member) = annotations_difference(reference, candidate, &field_pairs, &method_pairs)
    {
        return Some(ClassDifference::new(DifferenceKind::Annotations, member));
    }

    for kind in [
        DifferenceKind::Instructions,
        DifferenceKind::LineNumbers,
        DifferenceKind::LocalVariables,
        DifferenceKind::StackSizes,
    ] {
        if let Some((method, _)) = method_pairs
            .iter()
            .find(|(left, right)| code_differs(kind, left.code.as_ref(), right.code.as_ref()))
        {
            return Some(ClassDifference::new(kind, Some(method_key(method))));
        }
    }
    if reference.source_debug != candidate.source_debug {
        return Some(ClassDifference::new(DifferenceKind::SourceDebug, None));
    }
    let field_order = |class: &ClassNode| class.fields.iter().map(field_key).collect::<Vec<_>>();
    let method_order = |class: &ClassNode| class.methods.iter().map(method_key).collect::<Vec<_>>();
    if field_order(reference) != field_order(candidate)
        || method_order(reference) != method_order(candidate)
    {
        return Some(ClassDifference::new(DifferenceKind::MemberOrder, None));
    }
    None
}

const KOTLIN_METADATA: &str = "Lkotlin/Metadata;";
const DEBUG_METADATA: &str = "Lkotlin/coroutines/jvm/internal/DebugMetadata;";

fn field_key(field: &FieldNode) -> String {
    format!("{}: {}", field.name, field.desc)
}

fn method_key(method: &ClassMethod) -> String {
    format!("{} {}", method.name, method.desc)
}

fn by_key<T>(members: &[T], key: fn(&T) -> String) -> HashMap<String, &T> {
    members.iter().map(|member| (key(member), member)).collect()
}

/// The first of `members` that `other` does not declare.
fn only_in_one<T>(
    members: &[T],
    key: fn(&T) -> String,
    other: &HashMap<String, &T>,
) -> Option<String> {
    members
        .iter()
        .map(key)
        .find(|name| !other.contains_key(name))
}

/// Each of `members` beside the member of the other class with the same key, in `members`' order.
fn pairs<'a, T>(
    members: &'a [T],
    key: fn(&T) -> String,
    other: &HashMap<String, &'a T>,
) -> Vec<(&'a T, &'a T)> {
    members
        .iter()
        .filter_map(|member| other.get(&key(member)).map(|twin| (member, *twin)))
        .collect()
}

fn same_class_declaration(left: &ClassNode, right: &ClassNode) -> bool {
    left.major == right.major
        && left.access == right.access
        && left.name == right.name
        && left.signature == right.signature
        && left.super_name == right.super_name
        && left.interfaces == right.interfaces
        && left.source_file == right.source_file
        && left.outer_class == right.outer_class
        && left.inner_classes == right.inner_classes
        && left.deprecated == right.deprecated
}

fn same_field_declaration(left: &FieldNode, right: &FieldNode) -> bool {
    left.access == right.access
        && left.signature == right.signature
        && left.value == right.value
        && left.deprecated == right.deprecated
}

fn same_method_declaration(left: &ClassMethod, right: &ClassMethod) -> bool {
    left.access == right.access
        && left.signature == right.signature
        && left.exceptions == right.exceptions
        && left.parameters == right.parameters
        && left.annotation_default == right.annotation_default
        && left.deprecated == right.deprecated
        && left.code.is_some() == right.code.is_some()
}

fn annotation<'a>(annotations: &'a [Annotation], descriptor: &str) -> Option<&'a Annotation> {
    annotations
        .iter()
        .find(|annotation| annotation.desc == descriptor)
}

/// The class's annotations other than the two metadata annotations, which have their own kinds.
fn other_annotations(annotations: &[Annotation]) -> Vec<&Annotation> {
    annotations
        .iter()
        .filter(|annotation| {
            annotation.desc != KOTLIN_METADATA && annotation.desc != DEBUG_METADATA
        })
        .collect()
}

/// `Some(member)` when an annotation other than the metadata ones differs: `None` inside for the
/// class's own.
fn annotations_difference(
    reference: &ClassNode,
    candidate: &ClassNode,
    fields: &[(&FieldNode, &FieldNode)],
    methods: &[(&ClassMethod, &ClassMethod)],
) -> Option<Option<String>> {
    if other_annotations(&reference.visible_annotations)
        != other_annotations(&candidate.visible_annotations)
        || reference.invisible_annotations != candidate.invisible_annotations
    {
        return Some(None);
    }
    if let Some((field, _)) = fields.iter().find(|(left, right)| {
        left.visible_annotations != right.visible_annotations
            || left.invisible_annotations != right.invisible_annotations
    }) {
        return Some(Some(field_key(field)));
    }
    methods
        .iter()
        .find(|(left, right)| {
            left.visible_annotations != right.visible_annotations
                || left.invisible_annotations != right.invisible_annotations
                || left.visible_parameter_annotations != right.visible_parameter_annotations
                || left.invisible_parameter_annotations != right.invisible_parameter_annotations
        })
        .map(|(method, _)| Some(method_key(method)))
}

fn code_differs(
    kind: DifferenceKind,
    left: Option<&MethodNode>,
    right: Option<&MethodNode>,
) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    let (left, right) = (PositionedCode::of(left), PositionedCode::of(right));
    match kind {
        DifferenceKind::Instructions => !left.same_instructions(&right),
        DifferenceKind::LineNumbers => left.lines() != right.lines(),
        DifferenceKind::LocalVariables => left.locals() != right.locals(),
        DifferenceKind::StackSizes => {
            (left.method.max_stack, left.method.max_locals)
                != (right.method.max_stack, right.method.max_locals)
        }
        _ => false,
    }
}

/// A `LocalVariableTable` entry with its range as instruction positions.
#[derive(PartialEq, Eq)]
struct PositionedLocal<'a> {
    name: &'a str,
    desc: &'a str,
    slot: u16,
    start: Option<usize>,
    end: Option<usize>,
}

/// A method body with each label replaced by the index of the instruction it stands before.
struct PositionedCode<'a> {
    method: &'a MethodNode,
    instructions: Vec<&'a Insn>,
    positions: HashMap<LabelId, usize>,
}

impl<'a> PositionedCode<'a> {
    fn of(method: &'a MethodNode) -> PositionedCode<'a> {
        let mut instructions = Vec::new();
        let mut positions = HashMap::new();
        for node in &method.nodes {
            match node {
                Node::Label(label) => {
                    positions.insert(*label, instructions.len());
                }
                Node::Insn(insn) => instructions.push(insn),
                Node::Line { .. } => {}
            }
        }
        PositionedCode {
            method,
            instructions,
            positions,
        }
    }

    fn at(&self, label: LabelId) -> Option<usize> {
        self.positions.get(&label).copied()
    }

    fn same_instructions(&self, other: &PositionedCode<'_>) -> bool {
        let handlers = |code: &PositionedCode<'_>| {
            code.method
                .try_catch_blocks
                .iter()
                .map(|block| {
                    (
                        code.at(block.start),
                        code.at(block.end),
                        code.at(block.handler),
                        block.catch_type.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        self.instructions.len() == other.instructions.len()
            && self
                .instructions
                .iter()
                .zip(&other.instructions)
                .all(|(left, right)| self.same_instruction(left, other, right))
            && handlers(self) == handlers(other)
    }

    fn same_instruction(&self, left: &Insn, other: &PositionedCode<'_>, right: &Insn) -> bool {
        let targets = |code: &PositionedCode<'_>, labels: &[LabelId]| {
            labels
                .iter()
                .map(|label| code.at(*label))
                .collect::<Vec<_>>()
        };
        match (left, right) {
            (
                Insn::Jump { op, target },
                Insn::Jump {
                    op: other_op,
                    target: other_target,
                },
            ) => op == other_op && self.at(*target) == other.at(*other_target),
            (
                Insn::TableSwitch {
                    low,
                    high,
                    default,
                    labels,
                },
                Insn::TableSwitch {
                    low: other_low,
                    high: other_high,
                    default: other_default,
                    labels: other_labels,
                },
            ) => {
                (low, high) == (other_low, other_high)
                    && self.at(*default) == other.at(*other_default)
                    && targets(self, labels) == targets(other, other_labels)
            }
            (
                Insn::LookupSwitch {
                    default,
                    keys,
                    labels,
                },
                Insn::LookupSwitch {
                    default: other_default,
                    keys: other_keys,
                    labels: other_labels,
                },
            ) => {
                keys == other_keys
                    && self.at(*default) == other.at(*other_default)
                    && targets(self, labels) == targets(other, other_labels)
            }
            _ => left == right,
        }
    }

    fn lines(&self) -> Vec<(u16, Option<usize>)> {
        self.method
            .nodes
            .iter()
            .filter_map(|node| match node {
                Node::Line { line, start } => Some((*line, self.at(*start))),
                _ => None,
            })
            .collect()
    }

    fn locals(&self) -> Vec<PositionedLocal<'_>> {
        self.method
            .local_variables
            .iter()
            .map(|local| PositionedLocal {
                name: &local.name,
                desc: &local.desc,
                slot: local.slot,
                start: self.at(local.start),
                end: self.at(local.end),
            })
            .collect()
    }
}
