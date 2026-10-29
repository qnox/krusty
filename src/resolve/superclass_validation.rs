//! Class-supertype validity after the frontend has selected the exact superclass identity.

use super::abstract_obligations::UnimplementedDeclaration;
use super::{class_declaration_label, declaration_rendering, Checker};
use crate::ast::{ClassDecl, Decl, DeclId};
use crate::types::TypeName;

impl Checker<'_> {
    /// The class's own identity, its selected superclass, and whether that superclass is emitted
    /// in another unit than this file.
    fn selected_superclass(
        &self,
        class: &ClassDecl,
        current_owner: Option<TypeName>,
    ) -> Option<(TypeName, TypeName, bool)> {
        let owner = current_owner?;
        class.base_class.as_ref()?;
        let superclass = self
            .applied_declared_supertype(owner)?
            .kotlin_class_internal()?;
        Some((
            owner,
            superclass,
            self.resolved_type_name(superclass)
                .is_none_or(|shape| shape.source_file != Some(self.file_index)),
        ))
    }

    /// Validate the class's selected superclass. The abstract members it leaves unimplemented are
    /// checked here too, unless `abstract_members_later`: a body-local class's methods are
    /// normalized only on its own walk, and a method whose result its body infers implements its
    /// obligation only once that body is checked, so its walk calls
    /// [`Self::check_class_abstract_members`] after the methods instead.
    pub(super) fn validate_class_superclass(
        &mut self,
        declaration: DeclId,
        class: &ClassDecl,
        current_owner: Option<TypeName>,
        abstract_members_later: bool,
    ) {
        let Some((_, superclass, separate_emission)) =
            self.selected_superclass(class, current_owner)
        else {
            return;
        };

        let shape = self.resolver().classifier(superclass);
        let inheritance = shape.as_ref().map(|shape| shape.inheritance);
        crate::trace_compiler!(
            "resolve",
            "superclass capability class={} declaration={declaration:?} superclass={} separate_emission={separate_emission} inheritance={inheritance:?}",
            class.name,
            superclass.render(),
        );
        let cyclic_header = self.diags.diags.iter().any(|diagnostic| {
            diagnostic.file == self.file_index
                && class.span.lo <= diagnostic.span.lo
                && diagnostic.span.hi <= class.span.hi
                && diagnostic
                    .msg
                    .contains("cycle in supertypes and/or containing declarations")
        });
        let diagnostic = match &shape {
            _ if cyclic_header || class.is_enum() => None,
            Some(shape) if !shape.inheritance.is_extensible => {
                Some(if shape.kind == crate::libraries::TypeKind::Class {
                    (
                        class.base_class_span.unwrap_or(class.span),
                        "this type is final, so it cannot be extended.".into(),
                    )
                } else {
                    let superclass = superclass.render();
                    (
                        class.span,
                        format!(
                            "krusty: superclass '{superclass}' cannot be subclassed: it is not extensible"
                        ),
                    )
                })
            }
            _ => None,
        };
        if let Some((span, message)) = diagnostic {
            self.diags.error(span, message);
        }
        if !abstract_members_later {
            self.check_class_abstract_members(declaration, class, current_owner);
        }
    }

    /// Report the abstract members of the selected superclass's hierarchy that the concrete class
    /// does not implement.
    pub(super) fn check_class_abstract_members(
        &mut self,
        declaration: DeclId,
        class: &ClassDecl,
        current_owner: Option<TypeName>,
    ) {
        let Some((owner, superclass, separate_emission)) =
            self.selected_superclass(class, current_owner)
        else {
            return;
        };
        let inheritance = self
            .resolver()
            .classifier(superclass)
            .map(|shape| shape.inheritance);
        let checks_abstract_members = !class.is_enum()
            && separate_emission
            && inheritance.is_some_and(|shape| shape.is_abstract)
            // `sealed` is abstract: its own subclasses discharge members the sealed class leaves
            // open, including members inherited from a superclass emitted in another unit.
            && !class.modality.is_abstract();
        if !checks_abstract_members {
            return;
        }
        let unimplemented = self.unimplemented_abstract_members(owner);
        let source = self.fed_source();
        // kotlinc reports the members a class declares, then those an interface declares.
        let rendered = [(false, "abstract base class "), (true, "abstract ")]
            .into_iter()
            .filter_map(|(in_interface, kind)| {
                let members = unimplemented
                    .iter()
                    .filter(|member| member.in_interface == in_interface)
                    .map(|member| match &member.declaration {
                        UnimplementedDeclaration::Function(function) => {
                            declaration_rendering::declared_function(&source, function)
                        }
                        UnimplementedDeclaration::Property(property) => {
                            declaration_rendering::declared_property(&source, property)
                        }
                    })
                    .collect::<Vec<_>>();
                (!members.is_empty()).then_some((kind, members))
            })
            .collect::<Vec<_>>();
        drop(source);
        let label = self.abstract_obligation_label(declaration, class);
        for (kind, members) in rendered {
            self.diags.error(
                class.span,
                format!(
                    "class '{label}' is not abstract and does not implement {kind}{}:\n{}",
                    if members.len() == 1 {
                        "member"
                    } else {
                        "members"
                    },
                    members.join("\n")
                ),
            );
        }
    }

    /// The class as kotlinc names it in an abstract-obligation diagnostic: a body-local classifier
    /// is named from its outermost body-local container (`L.M`, `<anonymous>.M`), an object
    /// expression is `<anonymous>`, and any other class keeps its nested source name.
    fn abstract_obligation_label(&self, declaration: DeclId, class: &ClassDecl) -> String {
        let body_local = |id: DeclId| {
            self.file.is_local_declaration(id) || self.file.is_anonymous_object_class(id)
        };
        // Innermost first, up to the outermost body-local container.
        let mut chain = Vec::new();
        let mut current = Some(declaration);
        let mut outermost_body_local = None;
        while let Some(id) = current {
            let Decl::Class(classifier) = self.file.decl(id) else {
                break;
            };
            chain.push((id, classifier));
            if body_local(id) {
                outermost_body_local = Some(chain.len());
            }
            // A classifier nested in a local class is hoisted with that class's statement rather
            // than adopted by it.
            current = self.file.hoisted_classifier_owner(id).or_else(|| {
                self.file
                    .local_class_nested
                    .iter()
                    .find(|(_, nested)| nested.contains(&id))
                    .and_then(|(statement, _)| self.file.local_class_decls.get(statement))
                    .copied()
            });
        }
        let Some(length) = outermost_body_local else {
            return class.name.clone();
        };
        chain[..length]
            .iter()
            .rev()
            .map(|&(id, classifier)| {
                if self.file.is_anonymous_object_class(id) {
                    "<anonymous>"
                } else {
                    class_declaration_label(&classifier.name)
                }
            })
            .collect::<Vec<_>>()
            .join(".")
    }
}
