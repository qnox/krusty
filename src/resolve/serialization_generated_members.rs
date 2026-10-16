//! Frontend selection of the runtime declarations overridden by generated serializers.
//!
//! This runs while the semantic dependency provider is live. The backend receives only the four
//! selected return-value statuses and cannot reopen member lookup or recognize an ABI spelling.

use crate::libraries::{FnKind, FunctionInfo};
use crate::symbol_source::SymbolSource;
use crate::types::{ReturnValueStatus, SerializationGeneratedMemberStatuses, Ty, TypeName};

fn result_types_match(source: &dyn SymbolSource, left: Ty, right: Ty) -> bool {
    let left = left.canonical_semantic();
    let right = right.canonical_semantic();
    left == right
        || crate::symbol_resolver::resolution_subtype(source, left, right)
            && crate::symbol_resolver::resolution_subtype(source, right, left)
}

fn matches_shape(
    source: &dyn SymbolSource,
    function: &FunctionInfo,
    shape: &crate::plugins::serialization::GeneratedSerializerMemberShape,
) -> bool {
    if function.kind != FnKind::Member || function.context_count != 0 || function.flags.suspend {
        return false;
    }
    let parameters = function.semantic_params();
    let (formals, formal_bounds) = match function.generic_sig.as_ref() {
        Some(signature) => (
            signature.formals.as_slice(),
            signature.formal_bounds.as_slice(),
        ),
        None => (&[][..], &[][..]),
    };
    crate::symbol_resolver::override_input_shapes_match(
        source,
        crate::symbol_resolver::OverrideInputShape {
            params: &parameters,
            receiver: function
                .semantic_receiver()
                .filter(|_| function.is_extension()),
            formals,
            formal_bounds,
            context_count: function.context_count,
            suspend: function.flags.suspend,
        },
        crate::symbol_resolver::OverrideInputShape {
            params: &shape.parameters,
            receiver: None,
            formals: &[],
            formal_bounds: &[],
            context_count: 0,
            suspend: false,
        },
    ) && result_types_match(
        source,
        function.ret.apply(function.callable.ret),
        shape.result,
    )
}

/// Select one override slot through the runtime hierarchy in Kotlin's left-to-right depth-first
/// order. A Java declaration contributes no status, so selection continues to the declaration it
/// overrides; a Kotlin declaration's explicit `Unspecified` is itself the authoritative answer.
fn inherited_status(
    source: &dyn SymbolSource,
    root: Ty,
    shape: &crate::plugins::serialization::GeneratedSerializerMemberShape,
) -> Option<ReturnValueStatus> {
    let mut pending = vec![root];
    let mut seen = crate::fx_hash::HashSet::<TypeName>::default();
    while let Some(current) = pending.pop() {
        let Some(classifier) = current.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(classifier) {
            continue;
        }
        let declarations =
            crate::symbol_resolver::declared_member_declarations(source, current, shape.name);
        if let Some(status) = declarations
            .functions()
            .iter()
            .filter(|function| matches_shape(source, function, shape))
            .find_map(|function| function.flags.return_value_status)
        {
            return Some(status);
        }
        let mut supertypes = crate::symbol_resolver::direct_supertypes(source, current);
        supertypes.reverse();
        pending.extend(supertypes);
    }
    None
}

/// Resolve the exact four runtime declarations a generated serializer overrides. `None` means at
/// least one semantic slot is absent; callers must report that incomplete frontend contract rather
/// than manufacture `Unspecified` after Pass 1.
pub(crate) fn resolve(source: &dyn SymbolSource) -> Option<SerializationGeneratedMemberStatuses> {
    // Apply the interface to one concrete semantic type so every inherited class parameter is
    // substituted before shape comparison. Status does not vary with that argument.
    let serialized = Ty::obj("kotlin/String");
    let root = Ty::obj_args(
        crate::plugins::serialization::GENERATED_SERIALIZER_FQ,
        &[serialized],
    );
    let [serialize, deserialize, child_serializers, type_parameter_serializers] =
        crate::plugins::serialization::generated_serializer_member_shapes(serialized);
    Some(SerializationGeneratedMemberStatuses::new(
        inherited_status(source, root, &serialize)?,
        inherited_status(source, root, &deserialize)?,
        inherited_status(source, root, &child_serializers)?,
        inherited_status(source, root, &type_parameter_serializers)?,
    ))
}
