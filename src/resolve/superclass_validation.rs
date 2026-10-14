//! Class-supertype validity after the frontend has selected the exact superclass identity.

use super::abstract_obligations::UnimplementedDeclaration;
use super::{declaration_rendering, Checker};
use crate::ast::{ClassDecl, DeclId};
use crate::types::TypeName;

impl Checker<'_> {
    pub(super) fn validate_class_superclass(
        &mut self,
        declaration: DeclId,
        class: &ClassDecl,
        current_owner: Option<TypeName>,
    ) {
        let Some((owner, superclass, separate_emission)) = current_owner.and_then(|owner| {
            class.base_class.as_ref()?;
            self.resolved_body_local_supertypes
                .get(&owner)
                .and_then(|supertypes| supertypes.first())
                .and_then(|supertype| supertype.kotlin_class_internal())
                .or_else(|| self.direct_superclass_name(owner))
                .map(|superclass| {
                    (
                        owner,
                        superclass,
                        self.resolved_type_name(superclass)
                            .is_none_or(|shape| shape.source_file != Some(self.file_index)),
                    )
                })
        }) else {
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
        for (kind, members) in rendered {
            self.diags.error(
                class.span,
                format!(
                    "class '{}' is not abstract and does not implement {kind}{}:\n{}",
                    class.name,
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
}
