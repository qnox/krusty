//! Semantic resolution of source type references.
//!
//! This module owns function-type syntax, use-site projections, and type-alias application. Its
//! callers provide the lexical classifier table and type-parameter scope; later phases consume the
//! resulting semantic `Ty` and never repeat spelling-based resolution.

use crate::ast::{File, TypeRef};
use crate::diag::{DiagSink, Span};
use crate::types::{Ty, TypeName};

use super::{typeref_classifier, ClassNames, ClassifierBindingError, SymbolTable, TParams};

/// A bound-name → JVM-internal resolver over a `SymbolTable`: the merged class-name map, which binds
/// a bare contract/tparam-bound name to its internal for both module and classpath classes (and
/// already carries the Kotlin built-in → JVM mapping, `CharSequence` → `java/lang/CharSequence`).
/// Borrows only the copied `&SymbolTable`, so a caller can hold it while mutating its own state.
pub(crate) fn class_internal_resolver(
    syms: &SymbolTable,
) -> impl Fn(&str) -> Option<TypeName> + '_ {
    move |name: &str| syms.class_names.get(name)
}

pub(super) fn definitely_non_null_binding(binding: Ty) -> Ty {
    if binding == Ty::Null {
        Ty::obj("kotlin/Any")
    } else {
        binding.non_null()
    }
}

/// One normalized source arrow-function type shape.
#[derive(Clone, Copy)]
pub(super) struct FunctionTypeRefShape<'a> {
    pub(super) params: &'a [TypeRef],
    pub(super) ret: Option<&'a TypeRef>,
    pub(super) context_count: usize,
    pub(super) has_receiver: bool,
    pub(super) suspend: bool,
}

pub(super) fn function_type_ref_shape(ty: &TypeRef) -> Option<FunctionTypeRefShape<'_>> {
    if !ty.fun_params.is_empty() || ty.name == "<fun>" {
        return Some(FunctionTypeRefShape {
            params: &ty.fun_params,
            ret: ty.arg.as_deref(),
            context_count: (ty.fun_context_count as usize).min(ty.fun_params.len()),
            has_receiver: ty.fun_has_receiver(),
            suspend: ty.fun_suspend(),
        });
    }

    None
}

/// The phase-independent leaf of a `TypeRef`, resolved identically by signature collection, the
/// checker, and lowering: a function type, builtin scalar/`String`/`Unit`, or primitive array.
/// Classifiers, `Array<T>`, and type parameters remain the caller's name-lookup responsibility;
/// nullability is likewise applied by the phase-specific caller.
pub(crate) fn typeref_leaf(
    reference: &TypeRef,
    recurse: &mut dyn FnMut(&TypeRef) -> Ty,
) -> Option<Ty> {
    if let Some(shape) = function_type_ref_shape(reference) {
        let mut resolve_component = |component: &TypeRef| {
            if component.is_star_projection() {
                Ty::nullable(Ty::obj("kotlin/Any"))
            } else {
                recurse(component)
            }
        };
        let params = shape.params.iter().map(&mut resolve_component).collect();
        let ret = shape.ret.map(&mut resolve_component).unwrap_or(Ty::Unit);
        return Some(Ty::fun_with_shape(
            params,
            ret,
            shape.context_count,
            shape.has_receiver,
            shape.suspend,
        ));
    }
    if let Some(ty) = Ty::from_name(&reference.name) {
        return Some(ty);
    }
    Ty::primitive_array_element(&reference.name).map(Ty::array)
}

/// Apply one source type argument's projection after its leaf was resolved. A star retains its
/// existential readable upper bound without becoming an explicit `out` projection; metadata and
/// generic-signature consumers need that distinction.
pub(crate) fn projected_typeref_argument(
    argument: &TypeRef,
    resolved: Ty,
    star_upper_bound: Ty,
) -> Ty {
    if argument.is_star_projection() {
        Ty::star_projection(star_upper_bound)
    } else if argument.in_projection() {
        Ty::in_projection(resolved)
    } else if argument.out_projection() {
        Ty::out_projection(resolved)
    } else {
        resolved
    }
}

/// Bind classifier formals while computing use-site star upper bounds. A starred argument uses an
/// existential placeholder so another bound—or its own recursive F-bound—cannot expand that
/// parameter through its declaration bound again.
pub(crate) fn projected_classifier_argument_bindings(
    formals: &[String],
    arguments: &[Option<Ty>],
) -> crate::symbol_resolver::GSigBinds {
    let unbounded_star = Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")));
    formals
        .iter()
        .cloned()
        .zip(arguments.iter().copied())
        .map(|(formal, argument)| (formal, argument.unwrap_or(unbounded_star)))
        .collect()
}

fn apply_alias_expansion(
    classes: &ClassNames,
    name: &str,
    resolved: TypeName,
    args: &[Ty],
    import_path: bool,
    span: Span,
    diags: &mut DiagSink,
) -> Ty {
    let base = Ty::obj_args_name(resolved, args);
    // An import path resolves a declaration identity; it is not a use of the alias as a type and
    // therefore supplies no type arguments to the expansion.
    if import_path {
        return base;
    }
    let Some(alias) = classes.alias_expansion(name) else {
        return base;
    };
    // The template describes one classifier: the alias target. A spelling that resolved to a
    // nested/inherited classifier or a same-named class from another package keeps its own shape.
    // Only the use site knows which classifier won the complete lookup.
    if alias.target != resolved {
        return base;
    }
    if alias.formals.len() != args.len() {
        diags.error(
            span,
            format!(
                "wrong number of type arguments for type alias '{name}': expected {}, found {}.",
                alias.formals.len(),
                args.len()
            ),
        );
        return Ty::Error;
    }
    let bindings = alias
        .formals
        .iter()
        .cloned()
        .zip(args.iter().copied())
        .collect::<crate::symbol_resolver::GSigBinds>();
    let expansion = crate::types::ty_subst_alias_expansion(alias.expansion, &bindings);
    crate::trace_compiler!(
        "signature",
        "apply typealias spelling={} identity={:?} target={:?} formals={:?} arguments={args:?} template={:?} expansion={expansion:?}",
        name,
        alias.identity,
        alias.target,
        alias.formals,
        alias.expansion,
    );
    expansion
}

/// Resolve the receiver of an associated `companion fun/val Alias.name` declaration. This names a
/// classifier namespace, not an applied value type, so a generic alias needs no use-site arguments
/// and aliases with different fixed arguments associate with the same expanded classifier.
pub(super) fn associated_companion_receiver_ty(
    file: &File,
    receiver: &TypeRef,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    associated_companion_receiver_ty_from_spelling(
        receiver,
        file.alias_spellings.get(&receiver.span),
        classes,
        tparams,
        diags,
    )
}

pub(super) fn associated_companion_receiver_ty_from_spelling(
    receiver: &TypeRef,
    source_spelling: Option<&TypeRef>,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    let spelling = source_spelling.unwrap_or(receiver);
    if spelling.targs.is_empty() {
        if let Some(alias) = classes.alias_expansion(&spelling.name) {
            return Ty::obj_name(alias.target);
        }
    }
    ty_of_ref(receiver, classes, tparams, diags)
}

pub(super) fn ty_of_ref(
    reference: &TypeRef,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    ty_of_ref_with(reference, classes, tparams, diags)
}

pub(super) fn type_argument_of_ref(
    argument: &TypeRef,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    let ty = ty_of_ref_with(argument, classes, tparams, diags);
    projected_typeref_argument(argument, ty, Ty::nullable(Ty::obj("kotlin/Any")))
}

/// Apply a function-type alias template when the spelling has no classifier binding. Class-target
/// aliases are folded into the name map; an alias such as `typealias Listener = (String) -> Unit`
/// has no classifier to fold, so its recorded expansion is substituted here.
fn function_alias_expansion_use(
    reference: &TypeRef,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Option<Ty> {
    if reference.is_import() {
        return None;
    }
    let alias = classes.alias_expansion(&reference.name)?;
    let args = reference
        .targs
        .iter()
        .map(|argument| type_argument_of_ref(argument, classes, tparams, diags))
        .collect::<Vec<_>>();
    Some(apply_alias_expansion(
        classes,
        &reference.name,
        alias.target,
        &args,
        false,
        reference.span,
        diags,
    ))
}

pub(super) fn ty_of_ref_with(
    reference: &TypeRef,
    classes: &ClassNames,
    tparams: &TParams,
    diags: &mut DiagSink,
) -> Ty {
    if reference.definitely_non_null() && !tparams.contains(&reference.name) {
        diags.error(
            reference.span,
            "a definitely non-null type must use a type parameter on the left of '& Any'",
        );
        return Ty::Error;
    }
    let (resolved_classifier, failed_binding) = match classes.classifier_binding(&reference.name) {
        Ok(classifier) => (Some(classifier), None),
        Err(failure) => (None, Some(failure)),
    };
    let scoped = if tparams.contains(&reference.name) {
        Some(tparams.bound(&reference.name))
    } else {
        typeref_classifier(reference, resolved_classifier).map(|internal| {
            // A parameterless alias may still expand to a parameterized target whose arguments all
            // come from its right-hand side, so empty use-site arguments still enter this operation.
            let args = reference
                .targs
                .iter()
                .map(|argument| type_argument_of_ref(argument, classes, tparams, diags))
                .collect::<Vec<_>>();
            apply_alias_expansion(
                classes,
                &reference.name,
                internal,
                &args,
                reference.is_import(),
                reference.span,
                diags,
            )
        })
    };
    // Arrow/receiver-function syntax is already a semantic function type. The parser also retains
    // its physical `FunctionN` spelling for metadata/emission, but a real stdlib classifier under
    // that name must not replace the arrow's receiver/context/suspend shape.
    let function_syntax = !reference.fun_params.is_empty() || reference.name == "<fun>";
    let base = if function_syntax {
        typeref_leaf(reference, &mut |nested| {
            ty_of_ref_with(nested, classes, tparams, diags)
        })
        .expect("function syntax must produce a semantic function type")
    } else if let Some(ty) = scoped {
        ty
    } else if let Some(ty) = typeref_leaf(reference, &mut |nested| {
        ty_of_ref_with(nested, classes, tparams, diags)
    }) {
        ty
    } else if let Some(internal) = resolved_classifier.or_else(|| classes.get(&reference.name)) {
        if let Some(primitive) = internal.strip_prefix("__ty/") {
            Ty::from_name(&primitive).unwrap_or(Ty::Error)
        } else {
            // A classpath alias may name a target with a different argument list (`Lens<S, A>` =
            // `PLens<S, S, A, A>`). Substitute through the recorded template instead of attaching
            // the use-site arguments directly to the target classifier.
            let args = reference
                .targs
                .iter()
                .map(|argument| type_argument_of_ref(argument, classes, tparams, diags))
                .collect::<Vec<_>>();
            apply_alias_expansion(
                classes,
                &reference.name,
                internal,
                &args,
                reference.is_import(),
                reference.span,
                diags,
            )
        }
    } else if let Some(expanded) =
        (!matches!(failed_binding, Some(ClassifierBindingError::Ambiguous(_)),))
            .then(|| function_alias_expansion_use(reference, classes, tparams, diags))
            .flatten()
    {
        // A function-type alias has no classifier, but its source spelling still selects the
        // signature-collection template above. Ambiguous classifier lookup must never reach it.
        expanded
    } else {
        match failed_binding {
            Some(ClassifierBindingError::Ambiguous(_)) => {
                diags.error(
                    reference.span,
                    "overload resolution ambiguity between candidates:",
                );
            }
            Some(ClassifierBindingError::Unresolved(segment)) => {
                diags.error(reference.span, format!("unresolved reference '{segment}'."));
            }
            None => {
                diags.error(
                    reference.span,
                    format!("unresolved reference '{}'.", reference.name),
                );
            }
        }
        Ty::Error
    };
    let base = if reference.definitely_non_null() {
        match base {
            Ty::TyParam(name, bound) => Ty::ty_param(name, bound.non_null()),
            other => other,
        }
    } else {
        base
    };
    if reference.nullable() && base != Ty::Error {
        return Ty::nullable(base);
    }
    base
}
