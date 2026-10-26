//! How a value class named by `@Metadata` erases in a JVM descriptor, and the value class a
//! descriptor position recovers from it: the underlying for `X`, and for `X?` too when the
//! underlying's own null can stand for the absent value.

use super::meta_ids;
use crate::types::{Ty, TyKind, TypeName};

pub(super) fn meta_descriptor_position(
    index: usize,
    context_count: usize,
    extension: bool,
) -> usize {
    index + usize::from(extension && index >= context_count)
}

pub(super) fn meta_param_exact(
    name: Option<TypeName>,
    nullable: bool,
    desc: &Ty,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> bool {
    let Some(name) = name else {
        return super::ty_erases_to_object(*desc);
    };
    let ids = super::meta_ids();
    if name == ids.array {
        return matches!(desc.kind(), TyKind::Obj(n, args)
            if n == ids.array && args.first().copied().is_some_and(super::ty_erases_to_object));
    }
    if let Some(width) = super::prim_array_width(name) {
        return desc.obj_internal().and_then(super::prim_array_width) == Some(width);
    }
    if let Some(prim) = ids.prim.get(&name) {
        if nullable {
            return desc
                .obj_internal()
                .is_some_and(|actual| super::nullable_primitive_matches_descriptor(name, actual));
        }
        return match prim {
            u if u.is_unsigned() => *desc == *u || Some(*desc) == u.scalar_value_repr(),
            prim => desc == prim,
        };
    }
    if let Some(erased) = metadata_value_class_underlying(name, nullable, value_underlying) {
        return erased.non_null() == desc.non_null();
    }
    if name == ids.unit {
        *desc == Ty::Unit
    } else if name == ids.nothing {
        *desc == Ty::Nothing
    } else if matches!(*desc, Ty::String) {
        name == ids.string_kotlin || name == ids.string_java
    } else {
        desc.obj_internal().is_some_and(|desc_internal| {
            crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal(desc_internal, name)
        })
    }
}

/// The type-parameter declarations a metadata callable's types may name: the callable's own,
/// which shadow, then those of its enclosing classifiers, as the declaring class's metadata scope
/// lists them. Only the callable's own declare explicit type arguments; the enclosing ones are
/// bound evidence here and nothing else.
#[derive(Clone, Copy)]
pub(super) struct MetadataFormals<'a> {
    pub(super) callable: &'a crate::libraries::GenericSig,
    pub(super) enclosing: &'a [crate::jvm::metadata::ScopedTypeParameter],
}

impl MetadataFormals<'_> {
    /// The written bounds of the declaration `name` refers to, `None` when no declaration in
    /// scope has that name.
    fn bounds(&self, name: &str) -> Option<&[Ty]> {
        let callable = self.callable;
        callable
            .formals
            .iter()
            .position(|formal| formal == name)
            .map(|index| {
                callable
                    .formal_bounds
                    .get(index)
                    .map_or(&[][..], Vec::as_slice)
            })
            .or_else(|| {
                self.enclosing
                    .iter()
                    .find(|parameter| parameter.name == name)
                    .map(|parameter| parameter.bounds.as_slice())
            })
    }
}

/// The bound a metadata type parameter erases to on the JVM, kotlinc's `erasedUpperBound`: the
/// first written bound that is a class rather than an interface or a type parameter, else the
/// first written bound, followed through type-parameter bounds, an enclosing classifier's
/// included, to a classifier. Metadata keeps the written order, so the class bound need not lead.
/// An unbounded parameter's implicit bound is `Any?`. `None` for a parameter no declaration in
/// scope declares, or a cycle.
pub(super) fn metadata_primary_bound(
    ty: Ty,
    formals: MetadataFormals<'_>,
    is_interface: &dyn Fn(TypeName) -> bool,
) -> Option<Ty> {
    let mut current = ty;
    let mut seen = crate::fx_hash::HashSet::default();
    loop {
        let TyKind::TyParam(name, _) = current.non_null().kind() else {
            return Some(current);
        };
        if !seen.insert(name) {
            return None;
        }
        let bounds = formals.bounds(name.as_str())?;
        let next = bounds
            .iter()
            .copied()
            .find(|bound| {
                let concrete = bound.non_null();
                match concrete.kind() {
                    TyKind::TyParam(..) => false,
                    _ => concrete
                        .obj_internal()
                        .is_none_or(|classifier| !is_interface(classifier)),
                }
            })
            .or_else(|| bounds.first().copied())
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        let nullable = current.is_nullable() || next.is_nullable();
        current = if nullable {
            Ty::nullable(next.non_null())
        } else {
            next.non_null()
        };
    }
}

/// The classifier a metadata parameter's JVM descriptor names: its own class, or for a type
/// parameter the primary bound it erases to.
pub(super) fn metadata_parameter_class(
    parameter: Option<Ty>,
    function: &crate::jvm::metadata::MetaFn,
    enclosing: &[crate::jvm::metadata::ScopedTypeParameter],
    is_interface: &dyn Fn(TypeName) -> bool,
) -> Option<TypeName> {
    let parameter = parameter?;
    if !parameter.non_null().is_ty_param() {
        return parameter.non_null().obj_internal();
    }
    let callable = function.generic_sig.as_ref()?;
    metadata_primary_bound(
        parameter,
        MetadataFormals {
            callable,
            enclosing,
        },
        is_interface,
    )?
    .non_null()
    .obj_internal()
}

/// JVM carrier of a metadata type parameter whose primary class bound is a value class.
pub(super) fn metadata_value_class_bound_carrier(
    ty: Ty,
    formals: MetadataFormals<'_>,
    is_interface: &dyn Fn(TypeName) -> bool,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<Ty> {
    let class_bound = metadata_primary_bound(ty, formals, is_interface)?;
    let nullable = class_bound.is_nullable();
    let classifier = class_bound.non_null().obj_internal()?;
    let projected = metadata_value_class_underlying(classifier, nullable, value_underlying)?;
    Some(
        if nullable && !projected.is_nullable() && projected.is_reference() {
            Ty::nullable(projected)
        } else {
            projected
        },
    )
}

pub(super) fn metadata_carrier_matches(
    carrier: Ty,
    descriptor: &Ty,
    exact: bool,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> bool {
    let scalar = carrier
        .non_null()
        .scalar_value_repr()
        .filter(|_| !carrier.is_nullable());
    if let Some(scalar) = scalar {
        return descriptor.non_null() == scalar || descriptor.non_null() == carrier.non_null();
    }
    if exact {
        meta_param_exact(
            carrier.obj_internal(),
            carrier.is_nullable(),
            descriptor,
            value_underlying,
        )
    } else {
        super::meta_param_compat(
            carrier.obj_internal(),
            carrier.is_nullable(),
            descriptor,
            value_underlying,
        )
    }
}

/// The JVM representation recovered for a metadata-named value class. Keep unsigned normalization in
/// this single semantic adapter so top-level, member, exact, and compatible alignment cannot drift.
///
/// A NULLABLE value class erases to its underlying too when that underlying's own null can stand for
/// the absent value (`Tag?` over `String` is a nullable `String`); otherwise it stays the boxed class
/// in the descriptor and there is nothing to recover.
pub(super) fn metadata_value_class_underlying(
    name: TypeName,
    nullable: bool,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<Ty> {
    if nullable && !underlying_carries_null(name, value_underlying) {
        return None;
    }
    value_underlying(name).map(|underlying| {
        underlying
            .scalar_value_repr()
            .filter(|_| underlying.is_unsigned())
            .unwrap_or(underlying)
    })
}

/// Whether `name?` is its underlying on the JVM, as kotlinc erases it: the chain of underlyings
/// ends in a non-null reference, with no nullable link (whose null would be ambiguous) and no
/// primitive or unsigned scalar (which cannot hold null) on the way.
fn underlying_carries_null(
    name: TypeName,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> bool {
    let mut seen = crate::fx_hash::HashSet::default();
    let mut classifier = name;
    while seen.insert(classifier) {
        let Some(underlying) = value_underlying(classifier) else {
            return false;
        };
        if underlying.is_nullable() || !underlying.is_reference() || underlying.is_unsigned() {
            return false;
        }
        match underlying.obj_internal() {
            Some(inner) if value_underlying(inner).is_some() => classifier = inner,
            _ => return true,
        }
    }
    false
}

/// The VALUE CLASS each descriptor parameter position really has, per `@Metadata`.
///
/// A `@JvmInline value class` erases to its underlying in the descriptor, so the parsed parameter is
/// `J`/`Ljava/lang/String;` while the source type is `Duration`/`Tag`. Overload selection compares the
/// ARGUMENT's Kotlin type against the parameter, so without this a call passing the value class matches
/// nothing. Only a position whose metadata names a value class AND whose descriptor carries exactly
/// that value class's underlying is reported — anything else stays `None` and the erased type stands.
///
/// A NULLABLE value class whose descriptor carries the class itself (boxed) needs no recovery;
/// `metadata_value_class_underlying` already returns `None` for it. One erased to its underlying
/// (`Tag?` as `String`) is recovered as the nullable class.
pub(super) fn value_class_param_types(
    callable: &crate::jvm::metadata::MetaFn,
    desc_params: &[Ty],
    extension: bool,
    kept: usize,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Vec<Option<Ty>> {
    let mut out = vec![None; desc_params.len()];
    // Descriptor layout: [context params] [extension receiver] [value params].
    let context_count = callable.context_count();
    for (index, parameter) in callable.parameters().enumerate() {
        let position = meta_descriptor_position(index, context_count, extension);
        if position >= kept {
            break;
        }
        let (Some(name), Some(declared)) = (parameter.ty, desc_params.get(position)) else {
            continue;
        };
        let Some(underlying) =
            metadata_value_class_underlying(name, parameter.nullable(), value_underlying)
        else {
            continue;
        };
        if underlying.non_null() == declared.non_null() {
            // A value class krusty models as a SCALAR of its own is recovered as THAT scalar, never
            // as the boxed class. `UInt`/`ULong` ride in the JVM primitive slot of their carrier, so
            // a REFERENCE spelling here makes the lowerer box the argument (`kotlin/UInt.box-impl`)
            // into the erased descriptor slot (`I`/`J`) that takes it unboxed — a class file that
            // fails verification at load time. The scalar spelling is also the sharper one for the
            // overload selection this recovery exists for: it is exactly the type the checker gives
            // an unsigned argument. Every other value class (`Duration`, a user `Tag`) keeps the
            // class name: it is a reference on both sides, and the value-classes pass erases it at
            // the call.
            let recovered = meta_ids()
                .prim
                .get(&name)
                .copied()
                .unwrap_or_else(|| Ty::obj_name(name));
            out[position] = Some(if parameter.nullable() {
                Ty::nullable(recovered)
            } else {
                recovered
            });
        }
    }
    out
}

/// The VALUE CLASS a descriptor RETURN really has, per `@Metadata` — the return counterpart of
/// [`value_class_param_types`].
///
/// A value-class return erases exactly like a value-class parameter: the JVM method hands back the
/// UNDERLYING (`fun make(): K` → `make-<hash>()Ljava/lang/String;`) while `@Metadata` names `K`. The
/// call site needs BOTH halves. Knowing only the Kotlin return makes it treat the result as a BOXED
/// `K` and emit kotlinc's `checkcast K; K.unbox-impl()` over a `String` that is already the carrier —
/// a ClassCastException. Reporting the value class HERE is what marks the physical result as the
/// already-erased form, so the representation analysis leaves it alone.
///
/// Only a return whose metadata names a value class AND whose descriptor carries exactly that value
/// class's underlying is reported; anything else stays `None` and the erased type stands. A boxed
/// NULLABLE value class (the descriptor carries the class itself) gets `None` from
/// `metadata_value_class_underlying` and keeps its boxed handling; one erased to its underlying
/// (`Tag?` as `String`) is reported as the nullable class.
pub(super) fn value_class_return_type(
    callable: &crate::jvm::metadata::MetaFn,
    desc_ret: &Ty,
    value_underlying: &dyn Fn(TypeName) -> Option<Ty>,
) -> Option<Ty> {
    let name = callable.ret_class?;
    let underlying =
        metadata_value_class_underlying(name, callable.ret_nullable(), value_underlying)?;
    if underlying.non_null() != desc_ret.non_null() {
        return None;
    }
    // A value class krusty models as a SCALAR of its own is recovered as THAT scalar, never as the
    // boxed class — the same rule the parameter side applies, and for the same reason: `UInt`/`ULong`
    // ride in the JVM primitive slot of their carrier, so a REFERENCE spelling would make the lowerer
    // box a value the descriptor takes unboxed.
    let recovered = meta_ids()
        .prim
        .get(&name)
        .copied()
        .unwrap_or_else(|| Ty::obj_name(name));
    Some(if callable.ret_nullable() {
        Ty::nullable(recovered)
    } else {
        recovered
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::metadata::ScopedTypeParameter;

    fn signature(formals: &[(&str, Vec<Ty>)]) -> crate::libraries::GenericSig {
        crate::libraries::GenericSig {
            formals: formals.iter().map(|(name, _)| (*name).to_owned()).collect(),
            formal_bounds: formals.iter().map(|(_, bounds)| bounds.clone()).collect(),
            receiver: None,
            params: Vec::new(),
            ret: Ty::Unit,
            return_policy: Default::default(),
        }
    }

    fn enclosing(name: &str, bounds: Vec<Ty>) -> ScopedTypeParameter {
        ScopedTypeParameter {
            id: 0,
            name: name.to_owned(),
            bounds,
        }
    }

    fn interfaces(name: TypeName) -> bool {
        name == crate::types::type_name("owner/Marker")
    }

    /// The occurrence's carried bound is not evidence: `T` and `U` erase to their enclosing
    /// declarations' bounds even when every occurrence carries `Any`.
    #[test]
    fn an_enclosing_class_parameter_erases_to_its_declared_bound() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let callable = signature(&[]);
        let scope = [
            enclosing("T", vec![Ty::obj("owner/A")]),
            enclosing("U", vec![Ty::obj("owner/B")]),
        ];
        let formals = MetadataFormals {
            callable: &callable,
            enclosing: &scope,
        };

        assert_eq!(
            metadata_primary_bound(Ty::ty_param("T", any), formals, &interfaces),
            Some(Ty::obj("owner/A"))
        );
        assert_eq!(
            metadata_primary_bound(Ty::ty_param("U", any), formals, &interfaces),
            Some(Ty::obj("owner/B"))
        );
    }

    #[test]
    fn a_callable_bound_on_an_enclosing_parameter_erases_through_it() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let callable = signature(&[("R", vec![Ty::ty_param("T", any)])]);
        let scope = [enclosing(
            "T",
            vec![Ty::obj("owner/Marker"), Ty::obj("owner/A")],
        )];
        let formals = MetadataFormals {
            callable: &callable,
            enclosing: &scope,
        };

        assert_eq!(
            metadata_primary_bound(Ty::ty_param("R", any), formals, &interfaces),
            Some(Ty::obj("owner/A"))
        );
    }

    #[test]
    fn a_callable_parameter_shadows_an_enclosing_one_of_the_same_name() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let callable = signature(&[("T", vec![Ty::obj("owner/Marker"), Ty::obj("owner/B")])]);
        let scope = [enclosing("T", vec![Ty::obj("owner/A")])];
        let formals = MetadataFormals {
            callable: &callable,
            enclosing: &scope,
        };

        assert_eq!(
            metadata_primary_bound(Ty::ty_param("T", any), formals, &interfaces),
            Some(Ty::obj("owner/B"))
        );
    }

    #[test]
    fn an_unbounded_parameter_erases_to_nullable_any() {
        let callable = signature(&[("T", vec![])]);
        let formals = MetadataFormals {
            callable: &callable,
            enclosing: &[],
        };

        assert_eq!(
            metadata_primary_bound(Ty::ty_param("T", Ty::obj("owner/A")), formals, &interfaces),
            Some(Ty::nullable(Ty::obj("kotlin/Any")))
        );
    }

    #[test]
    fn an_undeclared_parameter_has_no_primary_bound() {
        let callable = signature(&[]);
        let formals = MetadataFormals {
            callable: &callable,
            enclosing: &[],
        };

        assert_eq!(
            metadata_primary_bound(Ty::ty_param("T", Ty::obj("owner/A")), formals, &interfaces),
            None
        );
    }
}
