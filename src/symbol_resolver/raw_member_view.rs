//! A member read through a Java raw receiver.
//!
//! Kotlin erases a raw receiver's members whole: each class formal reads as its erasure (the
//! receiver's `RawProjection` binding), each method formal as the erasure of its bound with no
//! inference left to do (`<U> U echo(U)` returns `Any!`), and every generic application in the
//! signature is itself raw (`List<P>` is a raw `List`). The receiver's bindings substitute first,
//! so an exact `Child<String>` keeps its applied members. A raw type's supertypes are raw too, so
//! an inherited member is read through the same view.

use crate::libraries::{FunctionInfo, GenericSig, LibraryCallable, PropertyInfo};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

use super::GSigBinds;

/// Whether members of `receiver` are read through Kotlin's raw view.
pub(super) fn is_raw_receiver(receiver: Ty) -> bool {
    receiver.non_null().is_raw_application()
}

/// Bind each method formal of `signature` to its erasure, its bound read through the receiver's
/// already-bound class formals.
pub(super) fn bind_method_formals(signature: &GenericSig, bindings: &mut GSigBinds) {
    let bounds = signature
        .formal_bounds
        .iter()
        .map(|bounds| {
            bounds
                .iter()
                .map(|bound| crate::types::ty_subst_keep_unbound(*bound, bindings))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for (formal, erasure) in signature
        .formals
        .iter()
        .zip(crate::types::formal_erasures(&signature.formals, &bounds))
    {
        bindings.insert(formal.clone(), Ty::raw_projection(erasure));
    }
}

/// Erase an already-specialized member function's signature. Its method formals stay declared, so
/// explicit type arguments are still accepted, but the erased signature no longer mentions them.
pub(super) fn erase_function(source: &dyn SymbolSource, function: &mut FunctionInfo) {
    erase_callable(source, &mut function.callable);
    if let Some(signature) = &mut function.generic_sig {
        erase_signature(source, signature);
    }
}

/// Erase an already-specialized member property's type and accessors.
pub(super) fn erase_property(source: &dyn SymbolSource, property: &mut PropertyInfo) {
    property.ty = erase(source, property.ty);
    erase_callable(source, &mut property.getter);
    property.getter.ret = property.ty;
    if let Some(setter) = &mut property.setter {
        erase_callable(source, setter);
    }
}

fn erase_callable(source: &dyn SymbolSource, callable: &mut LibraryCallable) {
    for parameter in &mut callable.params {
        *parameter = erase(source, *parameter);
    }
    callable.ret = erase(source, callable.ret);
    if let Some(signature) = &mut callable.generic_sig {
        erase_signature(source, signature);
    }
}

fn erase_signature(source: &dyn SymbolSource, signature: &mut GenericSig) {
    for parameter in &mut signature.params {
        *parameter = erase(source, *parameter);
    }
    signature.ret = erase(source, signature.ret);
}

/// `ty` read through a raw receiver: every generic application in it becomes raw.
pub(super) fn erase(source: &dyn SymbolSource, ty: Ty) -> Ty {
    crate::types::raw_member_type(ty, &|name: TypeName| {
        source.classifier(name).map(|classifier| {
            (
                classifier.type_params().clone(),
                classifier.type_param_bounds().clone(),
            )
        })
    })
}
