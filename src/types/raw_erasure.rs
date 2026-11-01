//! Erasure of a classifier's formals, as Kotlin reads a Java raw type.
//!
//! A raw occurrence applies each formal's erasure ([`formal_erasures`]); a member read through a
//! raw receiver erases its whole signature, so every generic application in it is itself raw
//! ([`raw_member_type`]). Both operate on declared formals and bounds; the JVM provider supplies
//! them from class headers, the resolver from the classifier record.

use super::{Ty, TyKind, TyParamKey, TypeName};

/// The erasure of each of `formals`, given each formal's declared bounds: its first bound, with
/// arguments that name one of `formals` starred (`T extends Comparable<T>` erases to
/// `Comparable<*>!`) and a bound that is itself one of `formals` erased in turn
/// (`B extends A`). A formal with no bound, or one in a bound cycle, erases to `Any?`. A Java
/// bound arrives flexible (`Object` is `Any!`), so its erasure stays flexible.
pub(crate) fn formal_erasures<F: TyParamKey>(formals: &[F], bounds: &[Vec<Ty>]) -> Vec<Ty> {
    (0..formals.len())
        .map(|index| formal_erasure(formals, bounds, index, &mut Vec::new()))
        .collect()
}

fn formal_erasure<F: TyParamKey>(
    formals: &[F],
    bounds: &[Vec<Ty>],
    index: usize,
    visiting: &mut Vec<usize>,
) -> Ty {
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let Some(&bound) = bounds.get(index).and_then(|bounds| bounds.first()) else {
        return any;
    };
    if visiting.contains(&index) {
        return any;
    }
    visiting.push(index);
    let erased = match bound.non_null().kind() {
        TyKind::TyParam(name, _) => match formals
            .iter()
            .position(|formal| formal.borrow() == name.as_str())
        {
            Some(named) => bound
                .rewrap_nullability(formal_erasure(formals, bounds, named, visiting).non_null()),
            None => any,
        },
        TyKind::Obj(..) => {
            bound.rewrap_nullability(star_formal_arguments(bound.non_null(), formals))
        }
        _ => bound,
    };
    visiting.pop();
    erased
}

fn star_formal_arguments<F: TyParamKey>(bound: Ty, formals: &[F]) -> Ty {
    let TyKind::Obj(name, arguments) = bound.kind() else {
        return bound;
    };
    if arguments.is_empty() {
        return bound;
    }
    Ty::obj_args_name(
        name,
        &arguments
            .iter()
            .map(|argument| {
                if super::ty_mentions_param(*argument, formals) {
                    Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))
                } else {
                    *argument
                }
            })
            .collect::<Vec<_>>(),
    )
}

/// The raw application of `classifier`: one [`TyKind::RawProjection`] per formal.
fn raw_application<F: TyParamKey>(classifier: TypeName, formals: &[F], bounds: &[Vec<Ty>]) -> Ty {
    Ty::obj_args_name(
        classifier,
        &formal_erasures(formals, bounds)
            .into_iter()
            .map(Ty::raw_projection)
            .collect::<Vec<_>>(),
    )
}

/// A classifier's declared formals and, parallel to them, each formal's bounds.
pub(crate) type DeclaredFormals = (Vec<String>, Vec<Vec<Ty>>);

/// `ty` as a member signature reads it through a raw receiver, once the receiver's formals are
/// bound to their erasures: every generic application becomes raw (`List<CharSequence!>` is a raw
/// `List`, `Comparable<*>` a raw `Comparable`), while an array keeps its shape around a raw-read
/// component. `formals_of` supplies a classifier's declared
/// formals and bounds; a classifier it does not know keeps its application.
pub(crate) fn raw_member_type(
    ty: Ty,
    formals_of: &dyn Fn(TypeName) -> Option<DeclaredFormals>,
) -> Ty {
    if let (true, Some(element)) = (ty.is_reference_array(), ty.array_elem()) {
        // An array is not a generic application to erase: it keeps its shape and the use-site
        // projection of its component, and only the component is read raw (`String[]` stays an
        // array of `String!`, `List<C>[]` becomes an array of raw `List`).
        let element = match element.kind() {
            TyKind::OutProjection(inner) => Ty::out_projection(raw_member_type(inner, formals_of)),
            TyKind::InProjection(inner) => Ty::in_projection(raw_member_type(inner, formals_of)),
            TyKind::StarProjection(_) => element,
            _ => raw_member_type(element, formals_of),
        };
        return Ty::obj_args_name(super::wk::array(), &[element]);
    }
    match ty.kind() {
        TyKind::Obj(name, arguments) if !arguments.is_empty() && !ty.is_raw_application() => {
            match formals_of(name) {
                Some((formals, bounds)) if formals.len() == arguments.len() => {
                    raw_application(name, &formals, &bounds)
                }
                _ => ty,
            }
        }
        TyKind::Nullable(inner) => Ty::nullable(raw_member_type(inner, formals_of)),
        TyKind::PlatformNullable(inner) => {
            Ty::platform_nullable(raw_member_type(inner, formals_of))
        }
        TyKind::MutabilityFlexible(inner) => {
            Ty::mutability_flexible(raw_member_type(inner, formals_of))
        }
        _ => ty,
    }
}
