//! Selected-constructor commitment and its origin-neutral declaration contract.

use crate::ast::ExprId;
use crate::libraries::{GenericSig, LibraryMember, LibraryType};
use crate::symbol_resolver::GSigBinds;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

use super::{Checker, ClassSig, ResolvedConstructor};

/// The declaration-owned generic shape shared by every selected constructor origin. Selection
/// normalizes source and provider declarations here; later publication consumes this record without
/// inspecting a constructor realization or querying a symbol provider again.
#[derive(Clone, Debug)]
pub(super) struct ConstructorDeclarationShape {
    pub(super) formals: Vec<crate::types::TyParamName>,
    pub(super) bounds: Vec<Ty>,
    pub(super) parameters: Vec<Ty>,
}

impl ConstructorDeclarationShape {
    pub(super) fn source(class: &ClassSig, parameters: Vec<Ty>) -> Self {
        Self {
            formals: crate::types::TyParamName::all(class.type_params()),
            bounds: class.type_param_bounds().clone(),
            parameters,
        }
    }

    pub(super) fn provider(classifier: &LibraryType, member: &LibraryMember) -> Self {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        Self {
            formals: crate::types::TyParamName::all(classifier.type_params()),
            bounds: classifier
                .type_param_bounds()
                .iter()
                .map(|bounds| bounds.first().copied().unwrap_or(any))
                .collect(),
            parameters: member.constructor_declaration_params().to_vec(),
        }
    }
}

/// Complete the argument-owned bindings of a dependency constructor before its expected result is
/// considered. Contravariant-only variables take their tightest recorded upper; a still-open class
/// variable may then be fixed through the constructed classifier's applied result supertype.
pub(super) fn initial_provider_bindings(
    source: &dyn SymbolSource,
    owner: TypeName,
    classifier: &LibraryType,
    expected: Option<Ty>,
    inferred: &GSigBinds,
    upper_only: &crate::fx_hash::HashSet<String>,
    tightest_upper: &GSigBinds,
) -> GSigBinds {
    let mut bindings = inferred.clone();
    for formal in upper_only {
        let binding = tightest_upper
            .get(formal.as_str())
            .copied()
            .unwrap_or(Ty::Nothing);
        bindings
            .entry(crate::types::TyParamName::new(formal))
            .or_insert(binding);
    }
    crate::symbol_resolver::seed_unbound_constructor_result_from_symbols(
        source,
        owner,
        classifier.type_params(),
        classifier.type_param_bounds(),
        expected,
        &mut bindings,
    );
    bindings
}

/// The already-checked selection evidence needed to reconsider nullable root bindings at the
/// constructor commitment boundary. Keeping these inputs together prevents that constructor-only
/// exception from becoming another generic-inference path.
pub(super) struct ExpectedConstructorBindingInputs<'a> {
    pub(super) source: &'a dyn SymbolSource,
    pub(super) signature: &'a GenericSig,
    pub(super) expected: Ty,
    pub(super) expected_bindings: &'a GSigBinds,
    pub(super) nullable_flexible_roots: &'a crate::fx_hash::HashSet<String>,
    pub(super) explicit_type_arguments: &'a [Ty],
    pub(super) actuals: &'a [(usize, usize, Ty, bool)],
    pub(super) vararg_index: Option<usize>,
}

impl Checker<'_> {
    /// Replace a provisional nullable binding only when an invariant expected constructor result
    /// owns that class variable and every written argument still fits the resulting Java flexible
    /// slot. Ordinary generic calls keep their nullable-root inference; this exception belongs to
    /// the declaration-owned result equality of a constructor.
    pub(super) fn retain_expected_flexible_constructor_bindings(
        &self,
        inputs: ExpectedConstructorBindingInputs<'_>,
        bindings: &mut GSigBinds,
    ) {
        let ExpectedConstructorBindingInputs {
            source,
            signature,
            expected,
            expected_bindings,
            nullable_flexible_roots,
            explicit_type_arguments,
            actuals,
            vararg_index,
        } = inputs;
        for formal in crate::symbol_resolver::expected_result_equality_formals(
            source,
            signature,
            expected,
            expected_bindings,
        ) {
            let Some(index) = signature
                .formals
                .iter()
                .position(|candidate| *candidate == formal)
            else {
                continue;
            };
            if !nullable_flexible_roots.contains(formal.as_str())
                || explicit_type_arguments
                    .get(index)
                    .is_some_and(|argument| *argument != Ty::Error)
            {
                continue;
            }
            let Some(&fixed) = expected_bindings.get(formal.as_str()) else {
                continue;
            };
            let mut trial = bindings.clone();
            trial.insert(crate::types::TyParamName::new(&formal), fixed);
            if !crate::symbol_resolver::generic_bindings_satisfy_bounds(
                signature,
                &trial,
                |actual, bound| self.generic_bound_admits(actual, bound),
            ) {
                continue;
            }
            let arguments_fit = actuals
                .iter()
                .all(|&(_, parameter_index, actual, whole_array)| {
                    let mut parameter = crate::symbol_resolver::ty_subst_keep_unbound(
                        signature.params[parameter_index],
                        &trial,
                    );
                    if vararg_index == Some(parameter_index) && !whole_array {
                        parameter = parameter.array_read_elem().unwrap_or(parameter);
                    }
                    self.receiver_is_assignable(actual, parameter)
                });
            if arguments_fit {
                *bindings = trial;
            }
        }
    }

    pub(super) fn discard_constructor_commitment(&mut self, call: ExprId) {
        self.resolved_constructors.remove(&call);
        self.discard_constructor_declaration_shape(call);
    }

    /// Commit the one semantic target and normalized declaration shape of a construction call.
    /// Rechecking during inference may have left an earlier callable probe for the same expression;
    /// that probe is not an alternative target and must not cross the checked boundary.
    pub(super) fn commit_constructor(
        &mut self,
        call: ExprId,
        target: ResolvedConstructor,
        declaration: ConstructorDeclarationShape,
    ) {
        self.resolved_calls.remove(&call);
        self.record_constructor_declaration_shape(call, declaration);
        self.resolved_constructors.insert(call, target);
    }
}
