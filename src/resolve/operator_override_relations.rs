//! Frontend-only overridden-symbol relations used by operator declaration checks.
//!
//! Ordinary declarations are published after stable signatures are complete and before bodies are
//! checked. A body-local classifier publishes the same relation from its normalized active-method
//! header before checking that method. Both are intentionally separate from backend-valid override
//! edges, whose stricter publication also validates written `override`, suspend compatibility and
//! covariant results.

use super::SymbolTable;
use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, ResolvedFunctionOverrideTarget,
    ResolvedModuleIndex, ResolvedOperatorEqualsOverride, ResolvedOperatorEqualsTarget,
};
use crate::libraries::{FnKind, FunctionInfo};
use crate::module_symbols::ModuleSymbols;
use crate::symbol_source::{CompositeSource, SymbolSource};
use crate::types::Ty;

struct DeclaredFunction {
    formals: Vec<String>,
    formal_bounds: Vec<Vec<Ty>>,
    parameters: Vec<Ty>,
    receiver: Option<Ty>,
    context_count: usize,
}

impl DeclaredFunction {
    fn from_function(function: &FunctionInfo) -> Self {
        let (formals, formal_bounds) = function.generic_sig.as_ref().map_or_else(
            || (Vec::new(), Vec::new()),
            |signature| (signature.formals.clone(), signature.formal_bounds.clone()),
        );
        Self {
            formals,
            formal_bounds,
            parameters: function.semantic_params().into_owned(),
            receiver: function
                .semantic_receiver()
                .filter(|_| function.is_extension()),
            context_count: function.context_count,
        }
    }
}

fn declared_function(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Option<DeclaredFunction> {
    let callable = index.callable_for_declaration(declaration)?;
    let signature = index.signature(declaration)?;
    let mut parameters = signature
        .parameters
        .iter()
        .map(|parameter| parameter.get())
        .collect::<Vec<_>>();
    let context_count = callable.shape.context_parameter_count as usize;
    if let Some(receiver) = callable.shape.extension_receiver {
        parameters.insert(context_count.min(parameters.len()), receiver.get());
    }
    let type_parameters = (0..)
        .map_while(|ordinal| index.type_parameter(declaration, ordinal))
        .filter_map(|parameter| {
            Some((
                index.type_parameter_semantic_name(parameter)?.to_owned(),
                index
                    .type_parameter_header(parameter)?
                    .bounds
                    .iter()
                    .map(|bound| bound.ty.get())
                    .collect(),
            ))
        })
        .collect::<Vec<(String, Vec<Ty>)>>();
    let (formals, formal_bounds) = type_parameters.into_iter().unzip();
    Some(DeclaredFunction {
        formals,
        formal_bounds,
        parameters,
        receiver: callable
            .shape
            .extension_receiver
            .map(crate::fir::ResolvedTy::get),
        context_count,
    })
}

fn any_equals_declarations(source: &dyn SymbolSource) -> Vec<FunctionInfo> {
    super::override_plans::declared_functions(
        source,
        Ty::obj_name(crate::types::wk::any()),
        "equals",
    )
    .into_iter()
    .filter(|function| {
        !function.flags.is_final
            && function.callable.declaration_owner
                == Some(crate::types::SemanticCallableOwner::Classifier(
                    crate::types::wk::any(),
                ))
            && function.callable.semantic_role
                == Some(crate::types::SemanticCallRole::KotlinAnyEquals)
    })
    .collect()
}

fn inputs_match(
    source: &dyn SymbolSource,
    inherited: &FunctionInfo,
    implementation: &DeclaredFunction,
) -> bool {
    if inherited.kind != FnKind::Member || implementation.receiver.is_some() {
        return false;
    }
    let inherited_inputs = inherited.semantic_params();
    let inherited_formals = inherited
        .generic_sig
        .as_ref()
        .map(|signature| signature.formals.as_slice())
        .unwrap_or_default();
    let inherited_bounds = inherited
        .generic_sig
        .as_ref()
        .map(|signature| signature.formal_bounds.as_slice())
        .unwrap_or_default();
    crate::symbol_resolver::override_input_shapes_match(
        source,
        crate::symbol_resolver::OverrideInputShape {
            params: &inherited_inputs,
            receiver: None,
            formals: inherited_formals,
            formal_bounds: inherited_bounds,
            context_count: inherited.context_count,
            suspend: false,
        },
        crate::symbol_resolver::OverrideInputShape {
            params: &implementation.parameters,
            receiver: None,
            formals: &implementation.formals,
            formal_bounds: &implementation.formal_bounds,
            context_count: implementation.context_count,
            // FIR records this relation before independently diagnosing a suspend mismatch.
            suspend: false,
        },
    )
}

fn target(
    index: &ResolvedModuleIndex,
    function: &FunctionInfo,
) -> Option<ResolvedOperatorEqualsTarget> {
    if let Some(declaration) = function.stable_declaration {
        let callable = index.callable_for_declaration(declaration)?;
        return Some(ResolvedOperatorEqualsTarget::Callable(
            ResolvedFunctionOverrideTarget::Module(callable.id),
        ));
    }
    if let Some(external) = function.callable.external_identity {
        return Some(ResolvedOperatorEqualsTarget::Callable(
            ResolvedFunctionOverrideTarget::External(external),
        ));
    }
    function
        .callable
        .compiler_core_declaration
        .map(ResolvedOperatorEqualsTarget::CompilerCore)
}

fn select_declared(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    implementation: &DeclaredFunction,
) -> Option<ResolvedOperatorEqualsOverride> {
    let inherited = any_equals_declarations(source)
        .into_iter()
        .find(|candidate| inputs_match(source, candidate, implementation))?;
    let Some(crate::types::SemanticCallableOwner::Classifier(overridden_owner)) =
        inherited.callable.declaration_owner
    else {
        return None;
    };
    Some(ResolvedOperatorEqualsOverride {
        overridden: target(index, &inherited)?,
        overridden_owner,
        overridden_semantic_role: inherited.callable.semantic_role,
        overridden_result: crate::fir::ResolvedTy::new(
            inherited
                .ret
                .apply(inherited.callable.ret)
                .canonical_semantic(),
        )
        .expect("a provider-normalized Any.equals result must be publishable"),
        overridden_suspend: inherited.flags.suspend || inherited.callable.suspend,
    })
}

pub(super) fn select_candidate(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    declaration: DeclarationId,
    implementation: &FunctionInfo,
) -> Option<ResolvedOperatorEqualsOverride> {
    if implementation.stable_declaration != Some(declaration)
        || implementation.callable.name != "equals"
    {
        return None;
    }
    let implementation = DeclaredFunction::from_function(implementation);
    select_declared(index, source, &implementation)
}

fn select(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    declarations: impl IntoIterator<Item = DeclarationId>,
) -> Vec<(DeclarationId, ResolvedOperatorEqualsOverride)> {
    declarations
        .into_iter()
        .filter_map(|declaration| {
            let header = index.declaration_header(declaration)?;
            if header.kind != DeclarationKind::Function
                || index.declaration_name(declaration) != Some("equals")
            {
                return None;
            }
            let implementation = declared_function(index, declaration)?;
            Some((
                declaration,
                select_declared(index, source, &implementation)?,
            ))
        })
        .collect()
}

fn publish(
    index: &mut ResolvedModuleIndex,
    selected: impl IntoIterator<Item = (DeclarationId, ResolvedOperatorEqualsOverride)>,
) {
    for (declaration, selected) in selected {
        index.publish_operator_equals_override(declaration, selected);
    }
}

pub(crate) fn publish_module(index: &mut ResolvedModuleIndex, table: &SymbolTable) {
    let selected = {
        let module = ModuleSymbols::new(table);
        let source = CompositeSource::new(vec![&module, table.libraries.as_ref()]);
        let owners = (0..index.declaration_count())
            .map(|raw| DeclarationId::from_raw(raw as u32))
            .filter(|owner| {
                let Some(header) = index.declaration_header(*owner) else {
                    return false;
                };
                matches!(
                    header.kind,
                    DeclarationKind::Classifier | DeclarationKind::EnumEntry
                ) && !header.flags.has(DeclarationFlags::LOCAL_CLASS)
            });
        let declarations = owners.flat_map(|owner| index.owned_declarations(owner).to_vec());
        select(index, &source, declarations)
    };
    publish(index, selected);
}

pub(crate) fn publish_checked_locals(
    index: &mut ResolvedModuleIndex,
    selected: &crate::fx_hash::HashMap<DeclarationId, ResolvedOperatorEqualsOverride>,
) -> Result<(), DeclarationId> {
    for &declaration in selected.keys() {
        let valid = index.declaration_header(declaration).is_some_and(|header| {
            header.kind == DeclarationKind::Function
                && header.owner.is_some_and(|owner| {
                    index.declaration_header(owner).is_some_and(|owner| {
                        owner.kind == DeclarationKind::Classifier
                            && owner.flags.has(DeclarationFlags::LOCAL_CLASS)
                    })
                })
        }) && index.signature(declaration).is_some()
            && index.callable_for_declaration(declaration).is_some();
        if !valid {
            return Err(declaration);
        }
    }
    for (&declaration, &selected) in selected {
        index.publish_operator_equals_override(declaration, selected);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
