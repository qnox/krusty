//! Expose anonymous implementation types only when an inferred declaration publishes them.

use crate::fir::{DeclarationFlags, DeclarationId, DiagnosticId};
use crate::fx_hash::HashSet;
use crate::symbol_resolver::{
    compose_type_position, GSigBinds, TypePosition, UnboundSpecialization,
};
use crate::symbol_source::{CompositeSource, SymbolSource};
use crate::types::{Ty, TyKind, TypeVariance};

use super::super::ProductionSignatureSemantics;

pub(super) fn declaration_result(
    semantics: &ProductionSignatureSemantics<'_>,
    declaration: DeclarationId,
    ty: Ty,
) -> Result<Ty, DiagnosticId> {
    let stub = semantics
        .headers
        .stub(declaration)
        .ok_or_else(ProductionSignatureSemantics::failure)?;
    // Local members retain their exact result even when written public. A private inline function
    // exposes its result because each call site materializes a distinct anonymous implementation.
    let expose = !stub.flags.has(DeclarationFlags::LOCAL_CLASS)
        && (stub.visibility != crate::types::Visibility::Private
            || stub.flags.has(DeclarationFlags::INLINE));
    if !expose || !contains_anonymous(semantics.table, semantics.headers, ty) {
        return Ok(ty);
    }
    let module = crate::module_symbols::ModuleSymbols::for_file(semantics.table, stub.source.raw());
    let source = CompositeSource::new(vec![&module, &*semantics.table.libraries]);
    let reject = |message: &str| {
        let span = semantics
            .headers
            .syntax
            .declaration(declaration)
            .and_then(|header| header.name_span)
            .expect("an inferred named declaration retains its diagnostic name span");
        semantics.record_source_diagnostic_at(declaration, stub.source, span, message.to_owned())
    };
    let parent_templates = |declaration| {
        semantics
            .classifier_parents
            .get(&declaration)?
            .parents
            .as_ref()
            .map(|parents| parents.declared.as_slice())
    };
    Projection {
        table: semantics.table,
        headers: semantics.headers,
        source: &source,
        reject: &reject,
        parent_templates: &parent_templates,
        visiting: HashSet::default(),
    }
    .approximate(ty, TypePosition::Out, &GSigBinds::default())
}

fn anonymous_class<'a>(
    table: &'a crate::resolve::SymbolTable,
    headers: &crate::fir::StreamedHeaderModule,
    ty: Ty,
) -> Option<&'a crate::resolve::ClassSig> {
    let TyKind::Obj(owner, _) = ty.kind() else {
        return None;
    };
    let classifier = table.class_by_type_name(owner)?;
    let declaration = classifier.stable_declaration?;
    headers
        .stub(declaration)?
        .flags
        .has(DeclarationFlags::ANONYMOUS_OBJECT)
        .then_some(classifier)
}

fn contains_anonymous(
    table: &crate::resolve::SymbolTable,
    headers: &crate::fir::StreamedHeaderModule,
    ty: Ty,
) -> bool {
    anonymous_class(table, headers, ty).is_some()
        || match ty.kind() {
            TyKind::Obj(_, arguments) => arguments
                .iter()
                .any(|argument| contains_anonymous(table, headers, *argument)),
            TyKind::Fun(signature) => {
                signature
                    .params
                    .iter()
                    .any(|parameter| contains_anonymous(table, headers, *parameter))
                    || contains_anonymous(table, headers, signature.ret)
            }
            TyKind::Nullable(inner)
            | TyKind::PlatformNullable(inner)
            | TyKind::InProjection(inner)
            | TyKind::OutProjection(inner)
            | TyKind::StarProjection(inner) => contains_anonymous(table, headers, inner),
            _ => false,
        }
}

struct Projection<'a> {
    table: &'a crate::resolve::SymbolTable,
    headers: &'a crate::fir::StreamedHeaderModule,
    source: &'a dyn SymbolSource,
    reject: &'a dyn Fn(&str) -> DiagnosticId,
    parent_templates: &'a dyn Fn(DeclarationId) -> Option<&'a [Ty]>,
    visiting: HashSet<Ty>,
}

impl Projection<'_> {
    fn reject(&self, message: &str) -> DiagnosticId {
        (self.reject)(message)
    }

    fn anonymous_parent(&self, ty: Ty) -> Result<Option<(Ty, GSigBinds)>, DiagnosticId> {
        let Some(class) = anonymous_class(self.table, self.headers, ty) else {
            return Ok(None);
        };
        let arity = class.type_params.len() + class.captured_type_parameters.type_params.len();
        if ty.type_args().len() != arity {
            return Err(
                self.reject("cannot publish an anonymous result with incomplete type arguments.")
            );
        }
        let classifier = self
            .source
            .classifier(class.internal_name())
            .expect("an authenticated source classifier retains its normalized declaration");
        let mut bindings = crate::symbol_resolver::classifier_bindings(&classifier, ty);
        // A star bound captured by this anonymous class is an existential result of its private
        // factory. Expose that captured slot through its readable bound, without rewriting stars in
        // other classifiers or losing a bound reconstructed from the complete application.
        for formal in &class.captured_type_parameters.type_params {
            if let Some(TyKind::StarProjection(bound)) = bindings.get(formal).map(|ty| ty.kind()) {
                bindings.insert(formal.clone(), Ty::out_projection(bound));
            }
        }
        let declaration = class
            .stable_declaration
            .expect("an authenticated anonymous classifier retains its declaration");
        let parents = (self.parent_templates)(declaration).ok_or_else(|| {
            self.reject(
                "cannot publish an anonymous result without its normalized declared parents.",
            )
        })?;
        match parents {
            [] => Ok(Some((Ty::obj("kotlin/Any"), bindings))),
            [parent] => {
                // Function shapes carry receiver/context/suspend facts that the nominal parent
                // classifier cannot express. Keep the template until positional approximation
                // consumes the complete captured application; structural substitution can create
                // invalid nested projections such as `in (out Number)`.
                Ok(Some((*parent, bindings)))
            }
            _ => {
                Err(self
                    .reject("right-hand side has an anonymous type. Specify the type explicitly."))
            }
        }
    }

    fn approximate(
        &mut self,
        ty: Ty,
        position: TypePosition,
        bindings: &GSigBinds,
    ) -> Result<Ty, DiagnosticId> {
        if !contains_anonymous(self.table, self.headers, ty)
            && !bindings
                .keys()
                .any(|formal| ty.type_parameter_occurrence_bound(formal).is_some())
        {
            return Ok(ty);
        }
        if let Some((parent, captured)) = self.anonymous_parent(ty)? {
            return match position {
                // An anonymous classifier is an actual nominal type, not an existential capture.
                // Negative result positions keep that identity; widening it would alter the
                // accepted inputs of a returned function or contravariant classifier.
                TypePosition::In => Ok(ty),
                TypePosition::Out => {
                    if !self.visiting.insert(ty) {
                        return Err(self.reject("cannot publish a recursive anonymous result."));
                    }
                    let result = self.approximate(parent, position, &captured);
                    self.visiting.remove(&ty);
                    result
                }
                TypePosition::Invariant => Err(self
                    .reject("cannot publish an anonymous result in an invariant type position.")),
            };
        }
        match ty.kind() {
            TyKind::TyParam(..) | TyKind::DefinitelyNotNull(_) => {
                let applied = crate::symbol_resolver::instantiate_slot(
                    self.source,
                    None,
                    ty,
                    bindings,
                    position,
                    UnboundSpecialization::Preserve,
                );
                if applied == ty {
                    Ok(ty)
                } else {
                    self.approximate(applied, position, bindings)
                }
            }
            TyKind::Obj(owner, arguments) => {
                let classifier = self.source.classifier(owner).ok_or_else(|| {
                    self.reject("cannot publish an anonymous result without its enclosing classifier declaration.")
                })?;
                let mut applied = Vec::with_capacity(arguments.len());
                for (index, argument) in arguments.iter().copied().enumerate() {
                    let variance = classifier.type_param_variances.get(index).copied()
                        .ok_or_else(|| self.reject("cannot publish an anonymous result without complete enclosing variance."))?;
                    let (inner, variance) = match argument.kind() {
                        TyKind::InProjection(inner) => (inner, TypeVariance::In),
                        TyKind::OutProjection(inner) => (inner, TypeVariance::Out),
                        TyKind::StarProjection(_) => {
                            applied.push(argument);
                            continue;
                        }
                        _ => (argument, variance),
                    };
                    // A directly captured use-site projection is already the declaration-owned
                    // view of an invariant argument. Keep its direction; compound argument shapes
                    // instead need their upper/lower interval computed below.
                    if matches!(argument.kind(), TyKind::TyParam(..))
                        && matches!(variance, TypeVariance::Invariant)
                    {
                        let view = crate::symbol_resolver::instantiate_slot(
                            self.source,
                            None,
                            argument,
                            bindings,
                            TypePosition::Invariant,
                            UnboundSpecialization::Preserve,
                        );
                        if view.projection_inner().is_some() {
                            if matches!(position, TypePosition::Out) {
                                applied.push(self.approximate(view, position, bindings)?);
                                continue;
                            }
                            return Ok(Ty::Nothing);
                        }
                    }
                    let projected = match compose_type_position(position, variance) {
                        TypePosition::Invariant => {
                            let upper = self.approximate(inner, TypePosition::Out, bindings)?;
                            let lower = self.approximate(inner, TypePosition::In, bindings)?;
                            if upper == lower {
                                upper
                            } else if matches!(position, TypePosition::Out) {
                                Ty::out_projection(upper)
                            } else if !bindings.iter().any(|(formal, bound)| {
                                bound.projection_inner().is_some()
                                    && inner.type_parameter_occurrence_bound(formal).is_some()
                            }) {
                                lower
                            } else {
                                return Ok(Ty::Nothing);
                            }
                        }
                        position => self.approximate(inner, position, bindings)?,
                    };
                    applied.push(match argument.kind() {
                        TyKind::InProjection(_) => Ty::in_projection(projected),
                        TyKind::OutProjection(_) => Ty::out_projection(projected),
                        _ => projected,
                    });
                }
                Ok(Ty::obj_args_name(owner, &applied))
            }
            TyKind::Fun(signature) => Ok(Ty::fun_with_shape(
                signature
                    .params
                    .iter()
                    .map(|parameter| {
                        self.approximate(
                            *parameter,
                            compose_type_position(position, TypeVariance::In),
                            bindings,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                self.approximate(signature.ret, position, bindings)?,
                signature.context_count,
                signature.has_receiver,
                signature.suspend,
            )),
            TyKind::Nullable(inner) => {
                Ok(Ty::nullable(self.approximate(inner, position, bindings)?))
            }
            TyKind::PlatformNullable(inner) => Ok(Ty::platform_nullable(
                self.approximate(inner, position, bindings)?,
            )),
            TyKind::InProjection(inner) => Ok(Ty::in_projection(self.approximate(
                inner,
                compose_type_position(position, TypeVariance::In),
                bindings,
            )?)),
            TyKind::OutProjection(inner) => Ok(Ty::out_projection(
                self.approximate(inner, position, bindings)?,
            )),
            _ => Ok(ty),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_anonymous_results_preserve_wrappers_and_declared_parameter_identities() {
        let source =
            "interface Parent<T>\nprivate fun <T> factory(value: T) = object : Parent<T> {}";
        let inputs =
            [crate::frontend::SourceInput::kotlin(source).with_file_stem("AnonymousWrappers")];
        let mut diagnostics = crate::diag::DiagSink::new();
        let files = [crate::frontend::parse_source_with_detected_features(
            source,
            &mut diagnostics,
        )];
        let headers = crate::fir::inventory_parsed_source_headers(&inputs, &files);
        let table = crate::resolve::collect_signatures_with_cp_headers(
            &files,
            &headers,
            Box::new(crate::libraries::EmptySymbolSource),
            &mut diagnostics,
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let anonymous: Vec<_> = table
            .classes
            .values()
            .filter(|class| {
                class.stable_declaration.is_some_and(|declaration| {
                    headers
                        .stub(declaration)
                        .is_some_and(|stub| stub.flags.has(DeclarationFlags::ANONYMOUS_OBJECT))
                })
            })
            .collect();
        assert_eq!(anonymous.len(), 1);
        let class = anonymous[0];
        assert!(class.type_params.is_empty());
        assert_eq!(class.captured_type_parameters.type_params.len(), 1);
        let formal = Ty::ty_param(
            &class.captured_type_parameters.type_params[0],
            class.captured_type_parameters.type_param_bounds[0],
        );
        let anonymous = Ty::obj_args_name(class.internal_name(), &[formal]);
        let parents = class
            .interfaces
            .iter_ids()
            .enumerate()
            .map(|(index, parent)| Ty::obj_args_name(parent, &class.interface_type_args[index]))
            .collect::<Vec<_>>();
        assert_eq!(parents.len(), 1);
        assert_eq!(parents[0].type_args(), &[formal]);
        let parent = parents[0];
        let declaration = class.stable_declaration.unwrap();
        let parent_templates = |owner| (owner == declaration).then_some(parents.as_slice());
        let module = crate::module_symbols::ModuleSymbols::for_file(&table, class.source_file);
        let source = CompositeSource::new(vec![&module, &*table.libraries]);
        let reject = |message: &str| panic!("valid source-derived result rejected: {message}");
        let mut projection = Projection {
            table: &table,
            headers: &headers,
            source: &source,
            reject: &reject,
            parent_templates: &parent_templates,
            visiting: HashSet::default(),
        };
        for (actual, expected) in [
            (anonymous, parent),
            (Ty::nullable(anonymous), Ty::nullable(parent)),
            (
                Ty::platform_nullable(anonymous),
                Ty::platform_nullable(parent),
            ),
            (formal, formal),
            (Ty::nullable(formal), Ty::nullable(formal)),
            (Ty::platform_nullable(formal), Ty::platform_nullable(formal)),
        ] {
            assert_eq!(
                projection.approximate(actual, TypePosition::Out, &GSigBinds::default()),
                Ok(expected),
                "{actual:?}"
            );
        }
        let definitely_non_null = formal.contributed_through_nullable_formal();
        assert!(
            matches!(definitely_non_null.kind(), TyKind::DefinitelyNotNull(inner) if inner == formal)
        );
        assert_eq!(
            projection.approximate(
                definitely_non_null,
                TypePosition::Out,
                &GSigBinds::default()
            ),
            Ok(definitely_non_null)
        );
    }
}
