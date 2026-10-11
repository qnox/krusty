//! Property overrides of a final declaration.
//!
//! An `override` property must name an overridable property of a direct supertype hierarchy. When
//! every non-private member property of that name is final, kotlinc reports the final owner at the
//! `override` modifier. The declaration view includes HIDDEN-deprecated properties: they remain
//! part of the override model even though ordinary lookup filters them.

use super::Checker;
use crate::ast::PropDecl;
use crate::libraries::PropKind;
use crate::types::{Ty, Visibility};

impl Checker<'_> {
    pub(super) fn check_final_property_overrides(
        &mut self,
        properties: &[PropDecl],
        direct_supertypes: &[Ty],
    ) {
        for property in properties {
            let Some(override_span) = property.override_span else {
                continue;
            };
            if property.receiver.is_some() {
                continue;
            }
            let source = self.fed_source();
            let declarations = direct_supertypes
                .iter()
                .flat_map(|supertype| {
                    crate::symbol_resolver::member_declarations_in_hierarchy(
                        &source,
                        *supertype,
                        &property.name,
                    )
                    .into_parts()
                    .1
                    .overloads
                })
                .filter(|candidate| {
                    candidate.kind == PropKind::Member
                        && candidate.visibility != Visibility::Private
                })
                .collect::<Vec<_>>();
            if declarations.iter().any(|candidate| !candidate.is_final) {
                continue;
            }
            let Some(owner) = declarations.first().map(|candidate| candidate.owner) else {
                continue;
            };
            self.diags.error(
                override_span,
                format!(
                    "'{}' in '{}' is final and cannot be overridden.",
                    property.name,
                    owner.segment().replace('$', ".")
                ),
            );
        }
    }
}
