//! The constructor overload selection chose, together with the classifier facts that choice
//! already had. Later result inference reads this record and does not open another classifier
//! or symbol source.

use super::generic_inference::constrain_constructor_result;
use super::{GSigBinds, SelectedConstructorCall};
use crate::libraries::{LibraryMember, LibraryType};
use crate::types::Ty;
use crate::types::TypeName;

/// The semantic declaration and classifier-owned generic facts selected as one resolver decision.
#[derive(Clone, Debug)]
pub struct SelectedConstructorDeclaration {
    pub(crate) declaration: LibraryMember,
    pub(crate) type_parameters: crate::types::TypeParameters<Vec<Vec<Ty>>>,
    /// Direct supertypes with this classifier's formals still symbolic.
    pub(crate) supertype_templates: Vec<Ty>,
}

pub(super) fn select_constructor_call(
    lib: &dyn crate::libraries::SemanticPlatform,
    src: &dyn crate::symbol_source::SymbolSource,
    internal: TypeName,
    args: &[super::CallArgKind],
) -> Option<SelectedConstructorCall> {
    let classifier = src.classifier(internal)?;
    super::select_constructor_call_from_type(lib, src, internal, &classifier, args)
}

pub(crate) fn capture(
    declaration: LibraryMember,
    classifier: &LibraryType,
) -> SelectedConstructorDeclaration {
    SelectedConstructorDeclaration {
        declaration,
        type_parameters: classifier.type_parameters.clone(),
        supertype_templates: classifier.supertype_templates.clone(),
    }
}

/// Both constructor realizations already carry the classifier facts captured at selection.
pub(crate) fn from_selected_call(call: &SelectedConstructorCall) -> SelectedConstructorDeclaration {
    match call {
        SelectedConstructorCall::Direct(selected) | SelectedConstructorCall::Platform(selected) => {
            selected.as_ref().clone()
        }
    }
}

/// Result of a constructor whose classifier is not in the frontend class table. Explicit type
/// arguments stay on the selected declaration. An empty application takes its arguments from the
/// expected type, including when that type is a direct supertype (`HashMap()` as `MutableMap<K, V>`).
pub(crate) fn unlisted_constructor_type(
    selected: &SelectedConstructorDeclaration,
    owner: TypeName,
    explicit_type_arguments: &[Ty],
    expected: Option<Ty>,
) -> Ty {
    if explicit_type_arguments.is_empty() {
        if let Some(result) = result_from_expected(selected, owner, expected) {
            return result;
        }
    }
    let member = &selected.declaration;
    if member.ret.obj_internal() == Some(owner) {
        member.ret
    } else {
        Ty::obj_name(owner)
    }
}

fn result_from_expected(
    selected: &SelectedConstructorDeclaration,
    owner: TypeName,
    expected: Option<Ty>,
) -> Option<Ty> {
    let formals = selected.type_parameters.type_params.as_slice();
    if formals.is_empty() {
        return None;
    }
    let mut bindings = GSigBinds::new();
    constrain_constructor_result(owner, formals, expected, &mut bindings);
    if !complete(formals, &bindings) {
        bind_direct_supertype(selected, expected, &mut bindings);
    }
    if !complete(formals, &bindings) {
        return None;
    }
    let arguments = formals
        .iter()
        .map(|formal| bindings.get(formal).copied().expect("complete"))
        .collect::<Vec<_>>();
    Some(Ty::obj_args_name(owner, &arguments))
}

fn complete(formals: &[String], bindings: &GSigBinds) -> bool {
    formals.iter().all(|formal| bindings.contains_key(formal))
}

fn bind_direct_supertype(
    selected: &SelectedConstructorDeclaration,
    expected: Option<Ty>,
    bindings: &mut GSigBinds,
) {
    let Some(expected) = expected.map(Ty::non_null) else {
        return;
    };
    let Some(expected_owner) = expected.obj_internal() else {
        return;
    };
    let expected_args = expected.type_args();
    let Some(template) = selected.supertype_templates.iter().find(|template| {
        template
            .non_null()
            .obj_internal()
            .is_some_and(|owner| owner == expected_owner)
    }) else {
        return;
    };
    for (formal, actual) in template
        .non_null()
        .type_args()
        .iter()
        .copied()
        .zip(expected_args.iter().copied())
    {
        if matches!(actual, Ty::StarProjection(_)) {
            continue;
        }
        let Ty::TyParam(name, _) = formal.non_null() else {
            continue;
        };
        bindings
            .entry(name.to_string())
            .or_insert(actual.projection_inner().unwrap_or(actual));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::LibraryMember;
    use crate::types::type_name;
    use crate::types::TypeParameters;

    #[test]
    fn an_expected_direct_supertype_fills_an_unlisted_constructor() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let owner = type_name("java/util/HashMap");
        let map = type_name("kotlin/collections/MutableMap");
        let mut declaration =
            LibraryMember::new("<init>".into(), Vec::new(), Ty::Unit, "()V".into());
        declaration.owner = Some(owner);
        declaration.ret = Ty::obj_args_name(owner, &[any, any]);
        let selected = SelectedConstructorDeclaration {
            declaration,
            type_parameters: TypeParameters::invariant(
                vec!["K".to_string(), "V".to_string()],
                vec![vec![any], vec![any]],
            ),
            supertype_templates: vec![Ty::obj_args_name(
                map,
                &[Ty::ty_param("K", any), Ty::ty_param("V", any)],
            )],
        };
        assert_eq!(
            unlisted_constructor_type(
                &selected,
                owner,
                &[],
                Some(Ty::obj_args_name(map, &[Ty::Int, Ty::String])),
            ),
            Ty::obj_args_name(owner, &[Ty::Int, Ty::String])
        );
    }
}
