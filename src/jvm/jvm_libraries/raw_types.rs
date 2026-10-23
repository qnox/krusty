//! Java raw type occurrences.
//!
//! Java code may name a generic classifier without arguments (`Box` for `Box<T extends
//! CharSequence>`). Kotlin reads that occurrence as the flexible type
//! `(Box<CharSequence!>..Box<*>?)`, and a raw non-static member class (`Outer.Inner`) as the same
//! interval over every formal it captures. The common model spells it as one
//! [`TyKind::RawProjection`] per formal, carrying the formal's erasure. This boundary produces it
//! from an authenticated raw occurrence and the referenced classifier's header: a Java
//! classifier's own `Signature` and its enclosing classes', or a Kotlin classifier's declared
//! formals. A classifier whose header is unavailable keeps its bare occurrence.

use super::*;
use crate::types::TyKind;

impl JvmLibraries {
    /// Replace each raw occurrence of a generic classifier in `ty` with its raw application.
    /// Explicit applications, type variables and non-generic classifiers are unchanged.
    pub(super) fn java_raw_occurrences(&self, ty: Ty) -> Ty {
        match ty.kind() {
            TyKind::Obj(internal, arguments) if arguments.is_empty() => {
                match self.raw_formal_erasures(internal) {
                    Some(erasures) if !erasures.is_empty() => Ty::obj_args_name(
                        internal,
                        &erasures
                            .into_iter()
                            .map(Ty::raw_projection)
                            .collect::<Vec<_>>(),
                    ),
                    _ => ty,
                }
            }
            TyKind::Obj(internal, arguments) => Ty::obj_args_name(
                internal,
                &arguments
                    .iter()
                    .map(|argument| self.java_raw_occurrences(*argument))
                    .collect::<Vec<_>>(),
            ),
            TyKind::Fun(signature) => Ty::fun_with_shape(
                signature
                    .params
                    .iter()
                    .map(|parameter| self.java_raw_occurrences(*parameter))
                    .collect(),
                self.java_raw_occurrences(signature.ret),
                signature.context_count,
                signature.has_receiver,
                signature.suspend,
            ),
            TyKind::Nullable(inner) => Ty::nullable(self.java_raw_occurrences(inner)),
            TyKind::PlatformNullable(inner) => {
                Ty::platform_nullable(self.java_raw_occurrences(inner))
            }
            TyKind::InProjection(inner) => Ty::in_projection(self.java_raw_occurrences(inner)),
            TyKind::OutProjection(inner) => Ty::out_projection(self.java_raw_occurrences(inner)),
            _ => ty,
        }
    }

    /// The erasure of every formal `internal` publishes, in the common classifier order (its own,
    /// then each enclosing class's for a non-static member class). `None` when the classifier is
    /// unknown.
    fn raw_formal_erasures(&self, internal: TypeName) -> Option<Vec<Ty>> {
        // Read only declaration headers here: a raw occurrence is met while a classifier's members
        // are being built, so asking for a complete classifier record could re-enter that build.
        if let Some(signature) = self.cp.builtin_class_signature_name(internal) {
            return Some(kotlin_formal_erasures(
                &signature.formals,
                &signature.formal_bounds,
            ));
        }
        let class = self.cp.find_name(internal)?;
        if class.meta.is_present() {
            let mut formals = Vec::new();
            let mut bounds = Vec::new();
            let mut current = Some(class);
            while let Some(class) = current {
                let parameters = &class.meta.class_type_parameters;
                formals.extend(parameters.type_params().iter().cloned());
                bounds.extend(parameters.type_param_bounds().iter().cloned());
                current = class
                    .meta
                    .is_inner
                    .then(|| class.inner_class_self())
                    .flatten()
                    .and_then(|entry| entry.outer.as_deref())
                    .and_then(|outer| self.cp.find_name(type_name(outer)));
            }
            return Some(kotlin_formal_erasures(&formals, &bounds));
        }
        let mut chain = vec![class];
        while let Some(outer) = chain
            .last()
            .and_then(|class| class.inner_class_self())
            .filter(|entry| entry.access & crate::jvm::classreader::ACC_STATIC == 0)
            .and_then(|entry| entry.outer.as_deref())
            .and_then(|outer| self.cp.find_name(type_name(outer)))
        {
            chain.push(outer);
        }
        let mut formals = Vec::new();
        let mut bounds = Vec::new();
        for class in &chain {
            let (own, own_bounds, _) = class
                .signature
                .as_deref()
                .map_or_else(|| (Vec::new(), Vec::new(), ""), parse_formals);
            formals.extend(own);
            bounds.extend(own_bounds);
        }
        Some(
            (0..formals.len())
                .map(|index| java_formal_erasure(&formals, &bounds, index, &mut Vec::new()))
                .collect(),
        )
    }
}

/// A Java formal's erasure as Kotlin's raw types read it: its first bound, flexible, with a
/// formal that bound names erased in turn (`B extends A` erases through `A`) and an argument
/// that names one of the classifier's formals starred (`T extends Comparable<T>` erases to
/// `Comparable<*>!`). An unbounded formal erases to `Any!`; a cycle stops at `Any!`.
fn java_formal_erasure(
    formals: &[String],
    bounds: &[Vec<Ty>],
    index: usize,
    visiting: &mut Vec<usize>,
) -> Ty {
    let any = Ty::platform_nullable(Ty::obj("kotlin/Any"));
    if visiting.contains(&index) {
        return any;
    }
    let Some(&bound) = bounds.get(index).and_then(|bounds| bounds.first()) else {
        return any;
    };
    visiting.push(index);
    let erased = match bound.non_null().kind() {
        TyKind::TyParam(name, _) => match formals.iter().position(|formal| *formal == *name) {
            Some(named) => java_formal_erasure(formals, bounds, named, visiting),
            None => any,
        },
        TyKind::Obj(..) => Ty::platform_nullable(star_formal_arguments(bound.non_null(), formals)),
        _ => bound,
    };
    visiting.pop();
    erased
}

/// A Kotlin formal's erasure: its first declared bound with formal-naming arguments starred, or
/// `Any?` when it declares none or is bounded by another formal.
fn kotlin_formal_erasures(formals: &[String], bounds: &[Vec<Ty>]) -> Vec<Ty> {
    (0..formals.len())
        .map(
            |index| match bounds.get(index).and_then(|bounds| bounds.first()) {
                Some(&bound) if !matches!(bound.non_null().kind(), TyKind::TyParam(..)) => {
                    bound.rewrap_nullability(star_formal_arguments(bound.non_null(), formals))
                }
                _ => Ty::nullable(Ty::obj("kotlin/Any")),
            },
        )
        .collect()
}

fn star_formal_arguments(bound: Ty, formals: &[String]) -> Ty {
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
                if crate::types::ty_mentions_param(*argument, formals) {
                    Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))
                } else {
                    *argument
                }
            })
            .collect::<Vec<_>>(),
    )
}
