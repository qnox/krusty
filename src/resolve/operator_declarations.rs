//! The shape kotlinc requires of an `operator` function (`OperatorFunctionChecks`), for the
//! conventions krusty checks: `hasNext`, the delegation conventions, and `equals`.

use super::value_class_checks::{star_projected_class, EqualsKind};
use super::*;

impl Checker<'_> {
    /// The checks of an `operator` declaration's shape that need its resolved result `ret`.
    /// `member` says whether it is a member of a classifier.
    pub(super) fn check_operator_declaration(&mut self, function: &FunDecl, ret: Ty, member: bool) {
        // A delegated-property convention is called from a GENERATED accessor, which has no scope
        // to fill an implicit context from. The declaration is rejected here, so the property that
        // uses it reports no applicable convention rather than reaching a call whose argument list
        // cannot be mapped onto the declaration's slots.
        if function.is_operator()
            && crate::resolve::delegated_properties::DELEGATE_CONVENTION_NAMES
                .contains(&function.name.as_str())
            && !crate::resolve::delegated_properties::is_usable_delegate_convention(
                true,
                function.context_count,
            )
        {
            self.diags.error(
                function
                    .context_span
                    .expect("a function with context parameters must retain its clause span"),
                "context parameters on delegation operators are unsupported.".to_string(),
            );
        }
        if function.is_operator()
            && function.name == "hasNext"
            && !matches!(ret.canonical_semantic(), Ty::Boolean | Ty::Error)
        {
            self.diags.error(
                function
                    .operator_span
                    .expect("an operator function must retain its modifier span"),
                "'operator' modifier is not applicable to function: must return 'Boolean'."
                    .to_string(),
            );
        }
        if function.is_operator() && function.name == "equals" && !member {
            self.report_inapplicable_operator(function, "must be a member function");
        }
    }

    /// An `operator fun equals` member must override `Any.equals`. With
    /// `CustomEqualsInValueClasses`, the typed equality of a value class qualifies too, and the
    /// message offers it. The members of an object expression are reported by kotlinc as a
    /// deprecation warning, which is not modelled.
    pub(super) fn check_operator_equals_members(
        &mut self,
        scope: &CheckerScope<'_>,
        declaration: DeclId,
        class: &ClassDecl,
        owner: Option<TypeName>,
    ) {
        if self
            .anonymous_lexical_scope
            .declarations
            .contains(&declaration)
        {
            return;
        }
        let custom_equals = self.file.language_gates.value_classes.custom_equals;
        let inline = self.is_inline_value_class(scope, class);
        for (index, method) in class.methods.iter().enumerate() {
            if !method.is_operator() || method.name != "equals" {
                continue;
            }
            let kind = self.equals_member_kind(declaration, class, owner, index, inline);
            let typed = custom_equals && kind == Some(EqualsKind::Typed);
            if kind == Some(EqualsKind::OfAny) || typed {
                continue;
            }
            let mut message = "must override 'equals()' in Any".to_string();
            if custom_equals && class.is_value {
                message.push_str(&format!(
                    " or define 'equals(other: {}): Boolean'",
                    star_projected_class(class)
                ));
            }
            self.report_inapplicable_operator(method, &message);
        }
    }

    fn report_inapplicable_operator(&mut self, function: &FunDecl, requirement: &str) {
        self.diags.error(
            function
                .operator_span
                .expect("an operator function must retain its modifier span"),
            format!("'operator' modifier is not applicable to function: {requirement}."),
        );
    }
}
