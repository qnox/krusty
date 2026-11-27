//! The hierarchy of a file's named declarations, as source positions.
//!
//! LSP document symbols, the KSP round and the Analysis API each need the same facts about a
//! source file: which declarations it contains, how they nest, and where each one's name and whole
//! range lie. This is that one structure, built from the parsed AST, so none of them re-lexes the
//! source to find a declaration again. Nodes are in source order and a node's parent precedes it.

use crate::ast::{ClassDecl, ClassKind, Decl, File, FunDecl, PropDecl, TypeAliasDecl};
use crate::diag::Span;

/// What a declaration is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutlineKind {
    Class,
    Interface,
    Enum,
    Annotation,
    Object,
    EnumEntry,
    Function,
    Property,
    TypeAlias,
}

/// One declaration: its kind, its simple name, where the name is and where the whole declaration
/// lies, and the index of the declaration that contains it.
#[derive(Clone, Copy, Debug)]
pub struct OutlineNode {
    pub kind: OutlineKind,
    pub name: &'static str,
    pub name_span: Span,
    pub range: Span,
    pub parent: Option<u32>,
}

/// The declarations of one file in source order.
#[derive(Debug, Default)]
pub struct DeclarationOutline {
    nodes: Vec<OutlineNode>,
}

impl DeclarationOutline {
    pub fn nodes(&self) -> &[OutlineNode] {
        &self.nodes
    }

    /// The outline of `file`'s named declarations. A local or anonymous declaration is not part of
    /// the file's structure and is left out.
    pub fn build(file: &File) -> Self {
        let anonymous = file
            .anonymous_object_classes
            .values()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let mut drafts = Vec::new();
        let mut classes = Vec::new();
        for &id in &file.decls {
            if file.is_local_declaration(id) || anonymous.contains(&id) {
                continue;
            }
            match file.decl(id) {
                Decl::Class(class) => {
                    let index = drafts.len();
                    drafts.push(Draft {
                        node: class_node(class),
                        container: None,
                    });
                    drafts.extend(class_member_drafts(class, index));
                    classes.push((index, class));
                }
                Decl::Fun(function) => drafts.push(Draft {
                    node: function_node(function),
                    container: None,
                }),
                Decl::Property(property) => drafts.push(Draft {
                    node: property_node(property),
                    container: None,
                }),
            }
        }
        drafts.extend(file.type_alias_decls.iter().map(|alias| Draft {
            node: type_alias_node(alias),
            container: None,
        }));
        // A nested classifier is a separate declaration of the file: its container is the
        // innermost other classifier whose source range encloses it.
        for &(index, class) in &classes {
            drafts[index].container = classes
                .iter()
                .filter(|&&(other, candidate)| {
                    other != index
                        && candidate.span.lo <= class.span.lo
                        && class.span.hi <= candidate.span.hi
                })
                .min_by_key(|&&(_, candidate)| candidate.span.hi - candidate.span.lo)
                .map(|&(other, _)| other);
        }
        Self::in_source_order(drafts)
    }

    fn in_source_order(drafts: Vec<Draft>) -> Self {
        let mut order = (0..drafts.len()).collect::<Vec<_>>();
        order.sort_by_key(|&index| {
            let node = &drafts[index].node;
            (node.name_span.lo, node.name_span.hi)
        });
        let mut position = vec![0u32; drafts.len()];
        for (sorted, &original) in order.iter().enumerate() {
            position[original] = sorted as u32;
        }
        let nodes = order
            .iter()
            .map(|&original| {
                let draft = &drafts[original];
                OutlineNode {
                    parent: draft.container.map(|container| position[container]),
                    ..draft.node
                }
            })
            .collect();
        Self { nodes }
    }
}

/// A node whose container is still the index of another draft.
struct Draft {
    node: OutlineNode,
    container: Option<usize>,
}

fn class_node(class: &ClassDecl) -> OutlineNode {
    let kind = if class.is_singleton() {
        OutlineKind::Object
    } else {
        match class.kind {
            ClassKind::Class => OutlineKind::Class,
            ClassKind::Interface => OutlineKind::Interface,
            ClassKind::Enum => OutlineKind::Enum,
            ClassKind::Annotation => OutlineKind::Annotation,
        }
    };
    OutlineNode {
        kind,
        name: crate::types::intern(class.name.rsplit('.').next().unwrap_or(&class.name)),
        name_span: class.name_span,
        range: class.span,
        parent: None,
    }
}

fn function_node(function: &FunDecl) -> OutlineNode {
    OutlineNode {
        kind: OutlineKind::Function,
        name: crate::types::intern(&function.name),
        name_span: function.name_span,
        range: function.declaration_span,
        parent: None,
    }
}

fn property_node(property: &PropDecl) -> OutlineNode {
    OutlineNode {
        kind: OutlineKind::Property,
        name: crate::types::intern(&property.name),
        name_span: property.name_span,
        range: property.declaration_span,
        parent: None,
    }
}

fn type_alias_node(alias: &TypeAliasDecl) -> OutlineNode {
    OutlineNode {
        kind: OutlineKind::TypeAlias,
        name: crate::types::intern(&alias.name),
        name_span: alias.name_span,
        range: alias.declaration_span,
        parent: None,
    }
}

fn class_member_drafts(class: &ClassDecl, container: usize) -> Vec<Draft> {
    let container = Some(container);
    let entries = class.enum_entries.iter().map(|entry| OutlineNode {
        kind: OutlineKind::EnumEntry,
        name: crate::types::intern(&entry.name),
        name_span: entry.span,
        range: entry.declaration_span,
        parent: None,
    });
    let functions = class.methods.iter().map(function_node);
    let properties = class.body_props.iter().map(property_node);
    let constructor_properties = class
        .props
        .iter()
        .filter(|property| property.is_property)
        .map(|property| OutlineNode {
            kind: OutlineKind::Property,
            name: crate::types::intern(&property.name),
            name_span: property.span,
            range: property.declaration_span,
            parent: None,
        });
    let aliases = class.type_aliases.iter().map(type_alias_node);
    entries
        .chain(constructor_properties)
        .chain(functions)
        .chain(properties)
        .chain(aliases)
        .map(|node| Draft { node, container })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{parse_source, LangFeatures};

    /// `(kind, name, parent's name)` in source order.
    fn outline(source: &str) -> Vec<(OutlineKind, &'static str, Option<&'static str>)> {
        let mut diagnostics = crate::diag::DiagSink::new();
        let file = parse_source(source, &LangFeatures::new(), &mut diagnostics);
        assert!(!diagnostics.has_errors(), "{:#?}", diagnostics.diags);
        let outline = DeclarationOutline::build(&file);
        outline
            .nodes()
            .iter()
            .map(|node| {
                (
                    node.kind,
                    node.name,
                    node.parent
                        .map(|parent| outline.nodes()[parent as usize].name),
                )
            })
            .collect()
    }

    #[test]
    fn nests_members_and_classifiers_in_source_order() {
        use OutlineKind::*;
        assert_eq!(
            outline(
                "val top = 1\nclass Outer {\n    val p = 2\n    fun m() {}\n    class Inner { fun deep() {} }\n    object Holder\n}\nenum class E { A, B; fun f() {} }\nfun last() {}\n"
            ),
            [
                (Property, "top", None),
                (Class, "Outer", None),
                (Property, "p", Some("Outer")),
                (Function, "m", Some("Outer")),
                (Class, "Inner", Some("Outer")),
                (Function, "deep", Some("Inner")),
                (Object, "Holder", Some("Outer")),
                (Enum, "E", None),
                (EnumEntry, "A", Some("E")),
                (EnumEntry, "B", Some("E")),
                (Function, "f", Some("E")),
                (Function, "last", None),
            ]
        );
    }

    #[test]
    fn includes_file_scope_aliases_and_constructor_properties() {
        use OutlineKind::*;
        assert_eq!(
            outline("typealias Label = String\nclass C(val x: Int, var y: String, plain: Int) {\n    typealias Inner = Int\n}\n"),
            [
                (TypeAlias, "Label", None),
                (Class, "C", None),
                (Property, "x", Some("C")),
                (Property, "y", Some("C")),
                (TypeAlias, "Inner", Some("C")),
            ]
        );
    }

    #[test]
    fn local_declarations_are_not_part_of_the_structure() {
        use OutlineKind::*;
        assert_eq!(
            outline("fun f() { class Local; val x = object { fun g() {} } }\n"),
            [(Function, "f", None)]
        );
    }
}
