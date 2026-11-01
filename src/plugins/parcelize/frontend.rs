//! The members kotlinc's `FirParcelizeDeclarationGenerator` declares, published before checking so
//! source code can call them and the class implements `Parcelable` through them.

use crate::libraries::PluginExpressionDeclaration;
use crate::plugins::{
    FrontendCallable, FrontendCallableOwner, FrontendClassCheckContext, FrontendClassContext,
    FrontendPluginDiagnostic,
};
use crate::types::{type_name, Ty, TyKind, TypeName};

pub(super) const DESCRIBE_CONTENTS: &str = "describeContents";
pub(super) const WRITE_TO_PARCEL: &str = "writeToParcel";

/// The identity under which common lowering records each generated member's function.
pub(super) fn declaration(operation: &'static str) -> PluginExpressionDeclaration {
    PluginExpressionDeclaration {
        plugin: "parcelize",
        operation,
    }
}

/// Declare `describeContents(): Int` and `writeToParcel(dest: Parcel, flags: Int)` on a class one
/// of `annotations` marks, unless it is abstract or sealed or declares the member itself.
pub(super) fn declare_members(
    annotations: &[TypeName],
    ctx: &FrontendClassContext<'_>,
    members: &mut Vec<FrontendCallable>,
) {
    if ctx.is_abstract
        || ctx.is_sealed
        || !ctx
            .annotations
            .iter()
            .any(|annotation| annotations.contains(annotation))
    {
        return;
    }
    let declared = |name: &str| {
        ctx.declared_functions
            .get(name)
            .map_or(&[][..], Vec::as_slice)
    };
    if !declared(DESCRIBE_CONTENTS)
        .iter()
        .any(|signature| signature.params.is_empty())
    {
        members.push(member(DESCRIBE_CONTENTS, Vec::new(), Ty::Int));
    }
    let parcel = type_name(super::PARCEL);
    if !declared(WRITE_TO_PARCEL).iter().any(|signature| {
        matches!(signature.params.as_slice(), [dest, flags]
            if matches!(dest.non_null().kind(), TyKind::Obj(name, _) if name == parcel)
                && *flags == Ty::Int)
    }) {
        members.push(member(
            WRITE_TO_PARCEL,
            vec![("dest", Ty::obj(super::PARCEL)), ("flags", Ty::Int)],
            Ty::Unit,
        ));
    }
}

fn member(name: &'static str, params: Vec<(&str, Ty)>, ret: Ty) -> FrontendCallable {
    FrontendCallable {
        owner: FrontendCallableOwner::Classifier,
        name: name.to_string(),
        param_names: params.iter().map(|(name, _)| name.to_string()).collect(),
        params: params.into_iter().map(|(_, ty)| ty).collect(),
        ret,
        generic_sig: None,
        plugin_expression: Some(declaration(name)),
        // Both members implement `Parcelable`'s.
        is_override: true,
    }
}

/// Reject a parcelized class's property whose type carries one of the runtime's type-use
/// annotations (`@RawValue`, `@WriteWith<P>`), which choose a serializer the port does not
/// implement yet. These are visible only in the declared type, so the check runs in the frontend.
pub(super) fn check_class(
    annotations: &[TypeName],
    ctx: &FrontendClassCheckContext<'_>,
    diagnostics: &mut Vec<FrontendPluginDiagnostic>,
) {
    if !ctx
        .annotations
        .iter()
        .any(|annotation| annotations.contains(annotation))
    {
        return;
    }
    for property in ctx.properties {
        if property
            .type_annotations
            .iter()
            .any(|&annotation| super::is_runtime_annotation(annotation))
        {
            diagnostics.push(FrontendPluginDiagnostic {
                span: property.declaration_span,
                message:
                    "krusty's Parcelize compiler plugin does not support a property type with \
                          a Parcelize annotation yet; compile this module with kotlinc"
                        .to_string(),
            });
        }
    }
}
