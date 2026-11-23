//! kotlinc's reserved-name loop of `FirValueClassDeclarationChecker`.
//!
//! An inline value class may not declare a non-abstract function named `box` or `unbox`, and,
//! unless `CustomEqualsInValueClasses` is enabled, `equals` or `hashCode` either:
//! `RESERVED_MEMBER_INSIDE_VALUE_CLASS` at each such function's name. A full value class reserves
//! no name. Only the class's own source declarations report here; the members the compiler
//! generates for a value class have no source. Like any kotlinc error, `@Suppress` on the
//! function or an enclosing element silences it.
//!
//! Not yet covered: kotlinc runs the same loop over the class's whole member scope, so a
//! non-abstract reserved function the class inherits from an interface without overriding it is
//! `RESERVED_MEMBER_FROM_INTERFACE_INSIDE_VALUE_CLASS` at the class's name.

use super::parameters::SelectedValueClassKind;
use super::*;

const BOX_AND_UNBOX: [&str; 2] = ["box", "unbox"];
const EQUALS_AND_HASH_CODE: [&str; 2] = ["equals", "hashCode"];

impl Checker<'_> {
    /// Report each own function of the value class `cl` (classifier `owner`) whose name is
    /// reserved inside it.
    pub(super) fn check_value_class_reserved_members(
        &mut self,
        scope: &CheckerScope<'_>,
        cl: &ClassDecl,
        owner: Option<TypeName>,
    ) {
        if self.discover_anonymous_captures
            || !cl.is_value
            || cl.singleton
            || cl.kind != ClassKind::Class
        {
            return;
        }
        let Some(owner) = owner else {
            return;
        };
        let Some(record) = self.resolver().classifier(owner) else {
            return;
        };
        let full = self
            .selected_value_class(owner, &record)
            .is_some_and(|selected| selected.kind == SelectedValueClassKind::Full);
        if full {
            return;
        }
        let equals_and_hash_code_reserved = !self.file.language.custom_equals_in_value_classes;
        for function in &cl.methods {
            if function.is_abstract() {
                continue;
            }
            let name = function.name.as_str();
            let reserved = BOX_AND_UNBOX.contains(&name)
                || (equals_and_hash_code_reserved && EQUALS_AND_HASH_CODE.contains(&name));
            if !reserved {
                continue;
            }
            let diagnostic = crate::diag::names::RESERVED_MEMBER_INSIDE_VALUE_CLASS;
            let depth = self.push_declaration_policies(
                scope,
                &function.annotations,
                &function.annotation_args,
            );
            if !self.suppresses_diagnostic(diagnostic.as_str()) {
                self.diags.error_named(
                    function.name_span,
                    diagnostic,
                    format!("Member name '{name}' is reserved for future releases."),
                );
            }
            self.active_lexical_policies.truncate(depth);
        }
    }
}
