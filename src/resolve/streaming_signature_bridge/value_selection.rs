//! Value selection for compact signature expressions.

use super::*;
use crate::resolve::implicit_rungs::ImplicitRung;

/// The value a name selected, with the enum entry it denotes when the selected candidate is one.
#[derive(Clone, Debug)]
pub(super) struct SelectedValue {
    pub(super) result: crate::fir::ResolvedTy,
    pub(super) enum_entry: Option<SelectedEnumEntry>,
}

/// The exact enum entry a value selection chose: its enum and its declared name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectedEnumEntry {
    pub(super) classifier: TypeName,
    pub(super) name: String,
}

impl SelectedValue {
    pub(super) fn plain(result: crate::fir::ResolvedTy) -> Self {
        Self {
            result,
            enum_entry: None,
        }
    }
}

impl ProductionSignatureSemantics<'_> {
    /// The value of enum entry `name` of `classifier`, which has the enum's type.
    fn enum_entry_value(
        classifier: TypeName,
        name: &str,
    ) -> Result<SelectedValue, crate::fir::DiagnosticId> {
        let result =
            crate::fir::ResolvedTy::new(Ty::obj_name(classifier)).map_err(|_| Self::failure())?;
        Ok(SelectedValue {
            result,
            enum_entry: Some(SelectedEnumEntry {
                classifier,
                name: name.to_owned(),
            }),
        })
    }

    /// Select the value `spelling` denotes, keeping the enum entry it selected, if any.
    pub(super) fn select_value_candidate(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        origin: crate::fir::OriginId,
        _expected: Option<crate::fir::ResolvedTy>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<SelectedValue, crate::fir::DiagnosticId> {
        crate::trace_compiler!(
            "signature",
            "select_value spelling={spelling} receivers={:?}",
            self.implicit_receivers(scope),
        );
        if spelling.contains('.') {
            // Bind the root once through the value tower. If it exists, every later segment is a
            // member of that semantic receiver; never reinterpret the root spelling as a package
            // or classifier when a later lookup fails.
            if let Some(receiver) =
                self.qualified_value_receiver(scope, spelling, origin, demand)?
            {
                return Ok(SelectedValue::plain(receiver));
            }
            if let Some((qualifier, name)) = spelling.rsplit_once('.') {
                if qualifier
                    .strip_prefix("this@")
                    .is_some_and(|label| self.enclosing_enum_entry_receiver(scope, label).is_some())
                {
                    if let Some(declaration) = self.enclosing_enum_entry_property(scope, name) {
                        return demand(declaration)
                            .map(|signature| signature.result)
                            .map(SelectedValue::plain);
                    }
                }
                if qualifier == "super"
                    || qualifier.starts_with("super<")
                    || qualifier.starts_with("super@")
                {
                    return self
                        .selected_super_member_property_result(scope, qualifier, name, demand)
                        .map(SelectedValue::plain);
                }
                if let Some(classifier) =
                    self.qualified_classifier_or_source_alias(scope, qualifier)
                {
                    // The enum qualifier's value is its companion, but `Enum.entries` is still the
                    // classifier property. `Enum.Companion.entries` names the companion member.
                    if let Some(result) =
                        self.prioritized_enum_entries_on_qualifier(scope, qualifier, name)
                    {
                        return crate::fir::ResolvedTy::new(result)
                            .map_err(|_| Self::failure())
                            .map(SelectedValue::plain);
                    }
                    if let Some(selected) =
                        self.select_qualified_associated_property(scope, classifier, name, demand)?
                    {
                        return Ok(selected);
                    }
                    // An enum entry is a value of its enum's static scope, named through the
                    // classifier qualifier once the root is known not to be a value.
                    if self.classifier_has_enum_entry(classifier, name) {
                        return Self::enum_entry_value(classifier, name);
                    }
                }
            }
            if let Ok(value) = self.qualified_receiver_ty(scope, spelling, origin, demand) {
                return Ok(SelectedValue::plain(value));
            }
            if let Some((qualifier, name)) = spelling.rsplit_once('.') {
                let package = crate::types::type_name(&qualifier.replace('.', "/"));
                let file = self
                    .headers
                    .scopes
                    .file(scope.source)
                    .ok_or_else(Self::failure)?;
                let access_package = self
                    .headers
                    .scopes
                    .path(file.package)
                    .iter()
                    .map(|segment| self.headers.lookup_names.get(*segment))
                    .collect::<Option<Vec<_>>>()
                    .map(|segments| crate::types::type_name(&segments.join("/")))
                    .ok_or_else(Self::failure)?;
                let module =
                    crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
                let property = crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
                    self.table.libraries.as_ref(),
                    &module,
                    std::slice::from_ref(&package),
                )
                .with_access_context(access_package, scope.source.raw(), Vec::new())
                .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, name, &[], &[])
                .and_then(crate::symbol_resolver::Symbol::value)
                .filter(|property| property.kind == crate::libraries::PropKind::TopLevel);
                if let Some(property) = property {
                    if let Some(signature) = self.demanded_source_signature(
                        Some(scope),
                        property.stable_declaration,
                        demand,
                    )? {
                        return Ok(SelectedValue::plain(signature.result));
                    }
                    return crate::fir::ResolvedTy::new(property.ty)
                        .map_err(|_| Self::failure())
                        .map(SelectedValue::plain);
                }
            }
            return Err(Self::failure());
        }
        if spelling == "this" {
            return self
                .implicit_receivers(scope)
                .into_iter()
                .next()
                .and_then(|receiver| crate::fir::ResolvedTy::new(receiver).ok())
                .ok_or_else(Self::failure)
                .map(SelectedValue::plain);
        }
        // `super.p` / `super<C>.p` in an inferred initializer. The receiver of a super access is the
        // SUPERTYPE, not the current class: without this the whole module's signatures decline with
        // no diagnostic. An explicit qualifier names the supertype directly; a bare `super` takes the
        // class supertype, which is the only one whose members a super access can read.
        if spelling == "super" || spelling.starts_with("super<") || spelling.starts_with("super@") {
            let receivers = self.implicit_receivers(scope);
            let label = spelling.rsplit_once('@').map(|(_, label)| label);
            let current = match label {
                Some(label) => receivers.into_iter().find(|receiver| {
                    receiver.obj_internal().is_some_and(|classifier| {
                        self.classifier_source_spelling(classifier) == label
                    })
                }),
                None => receivers.into_iter().next(),
            }
            .ok_or_else(Self::failure)?;
            let base_spelling = spelling.split('@').next().unwrap_or(spelling);
            if let Some(qualifier) = base_spelling
                .strip_prefix("super<")
                .and_then(|rest| rest.strip_suffix('>'))
            {
                let internal = self
                    .qualified_classifier(scope, qualifier)
                    .ok_or_else(Self::failure)?;
                return crate::fir::ResolvedTy::new(Ty::obj_name(internal))
                    .map_err(|_| Self::failure())
                    .map(SelectedValue::plain);
            }
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let source = &source as &dyn crate::symbol_source::SymbolSource;
            let supertypes = crate::symbol_resolver::direct_supertypes(source, current);
            let selected = self
                .enum_entry_direct_super(scope, current)
                .or_else(|| {
                    supertypes
                        .iter()
                        .copied()
                        .find(|supertype| {
                            supertype.kotlin_class_internal().is_some_and(|internal| {
                                source
                                    .classifier(internal)
                                    .is_some_and(|declaration| !declaration.is_interface())
                            })
                        })
                        .or_else(|| supertypes.first().copied())
                })
                .ok_or_else(Self::failure)?;
            return crate::fir::ResolvedTy::new(selected)
                .map_err(|_| Self::failure())
                .map(SelectedValue::plain);
        }
        if let Some(label) = spelling.strip_prefix("this@") {
            if let Some(receiver) = self.enclosing_enum_entry_receiver(scope, label) {
                return crate::fir::ResolvedTy::new(receiver)
                    .map_err(|_| Self::failure())
                    .map(SelectedValue::plain);
            }
            let labels_current_extension = self.headers.stub(scope.owner).is_some_and(|stub| {
                stub.lookup_name
                    .and_then(|name| self.headers.lookup_names.get(name))
                    == Some(label)
            });
            if labels_current_extension {
                if let Some(receiver) = self.declaration_extension_receiver(scope.owner) {
                    return crate::fir::ResolvedTy::new(receiver)
                        .map_err(|_| Self::failure())
                        .map(SelectedValue::plain);
                }
            }
            return self
                .implicit_receivers(scope)
                .into_iter()
                .find(|receiver| {
                    receiver.obj_internal().is_some_and(|classifier| {
                        self.classifier_source_spelling(classifier) == label
                    })
                })
                .and_then(|receiver| crate::fir::ResolvedTy::new(receiver).ok())
                .ok_or_else(Self::failure)
                .map(SelectedValue::plain);
        }
        // Primary-constructor parameters occupy the lexical initializer rung in front of dispatch
        // properties. In `class C(vararg xs: Int) { val xs = xs }`, selecting the property first
        // creates a false self-cycle; the right-hand `xs` is the normalized `IntArray` parameter.
        if let Some(parameter) =
            self.demanded_enclosing_constructor_parameter(scope, spelling, demand)?
        {
            return crate::fir::ResolvedTy::new(parameter)
                .map_err(|_| Self::failure())
                .map(SelectedValue::plain);
        }
        if let Some(capture) = self.enclosing_capture_type(scope, spelling) {
            return crate::fir::ResolvedTy::new(capture)
                .map_err(|_| Self::failure())
                .map(SelectedValue::plain);
        }
        if let Some(declaration) = self.enclosing_enum_entry_property(scope, spelling) {
            return demand(declaration)
                .map(|signature| signature.result)
                .map(SelectedValue::plain);
        }
        for rung in self.implicit_rungs(scope) {
            let receiver = match rung {
                ImplicitRung::PrioritizedClassifierProperties(owner) => {
                    if let Some(result) =
                        self.selected_implicit_classifier_property(scope, owner, spelling)
                    {
                        return crate::fir::ResolvedTy::new(result)
                            .map_err(|_| Self::failure())
                            .map(SelectedValue::plain);
                    }
                    continue;
                }
                crate::resolve::implicit_rungs::ImplicitRung::StaticScope(classifier) => {
                    if let Some(selected) =
                        self.select_static_scope_property(scope, classifier, spelling, demand)?
                    {
                        return Ok(selected);
                    }
                    continue;
                }
                crate::resolve::implicit_rungs::ImplicitRung::Receiver(receiver) => receiver,
            };
            if let Some(result) =
                self.selected_member_property_type(scope, receiver, spelling, demand)?
            {
                return Ok(SelectedValue::plain(result));
            }
            // Compile-time constants are declaration facts on the receiver classifier, not
            // property accessor candidates. This is the same provider-normalized constant channel
            // used for qualified reads; it also applies to a bare read inside an extension on a
            // companion receiver (`fun Int.Companion.max() = MAX_VALUE`).
            if let Some(internal) = receiver.non_null().obj_internal() {
                if let Some(constant) = self
                    .table
                    .libraries
                    .classifier(internal)
                    .and_then(|declaration| declaration.constants.get(spelling).cloned())
                {
                    return crate::fir::ResolvedTy::new(constant.ty)
                        .map_err(|_| Self::failure())
                        .map(SelectedValue::plain);
                }
            }
            if let Ok((result, member, extension)) = self.with_resolver(scope, |resolver| {
                let crate::symbol_resolver::Symbol::Member(facets) = resolver.resolve_symbol(
                    crate::symbol_resolver::SymRecv::Value(receiver),
                    spelling,
                    &[],
                    &[],
                )?
                else {
                    return None;
                };
                if let Some(property) = facets.extension_property {
                    return Some((property.ty, None, Some(property)));
                }
                facets.read.map(|property| {
                    let member = property.member;
                    (property.ret, Some(member), None)
                })
            }) {
                if let Some(member) = member.as_ref() {
                    if let Some(signature) =
                        self.demanded_member_signature(member.stable_declaration, demand)?
                    {
                        return self
                            .apply_demanded_member(receiver, member, &signature, &[], &[])
                            .map(SelectedValue::plain);
                    }
                }
                if let Some(extension) = extension {
                    if let Some(signature) =
                        self.demanded_source_signature(None, extension.stable_declaration, demand)?
                    {
                        return Ok(SelectedValue::plain(signature.result));
                    }
                }
                return crate::fir::ResolvedTy::new(result)
                    .map_err(|_| Self::failure())
                    .map(SelectedValue::plain);
            }
            for dispatch_receiver in self.signature_dispatch_receivers(scope) {
                let selected = self.member_extension_property_for(
                    scope,
                    receiver,
                    dispatch_receiver,
                    spelling,
                );
                let (result, declaration) = match selected {
                    Ok(Some(selected)) => selected,
                    Ok(None) => continue,
                    Err(()) => {
                        return Err(self.record_ambiguous_member(scope.owner, origin, spelling));
                    }
                };
                crate::trace_compiler!(
                    "signature",
                    "member extension property selected name={spelling} extension={receiver:?} dispatch={dispatch_receiver:?} result={result:?} declaration={declaration:?}",
                );
                if let Some(declaration) = declaration {
                    if self
                        .headers
                        .stub(declaration)
                        .is_some_and(|stub| stub.signature_inference.is_some())
                    {
                        return demand(declaration)
                            .map(|signature| signature.result)
                            .map(SelectedValue::plain);
                    }
                }
                return crate::fir::ResolvedTy::new(result)
                    .map_err(|_| Self::failure())
                    .map(SelectedValue::plain);
            }
        }
        for classifier in self.lexical_class_names(scope) {
            if self.classifier_has_enum_entry(classifier, spelling) {
                return Self::enum_entry_value(classifier, spelling);
            }
            if let Some(result) =
                self.selected_implicit_classifier_property(scope, classifier, spelling)
            {
                return crate::fir::ResolvedTy::new(result)
                    .map_err(|_| Self::failure())
                    .map(SelectedValue::plain);
            }
        }
        // A provider may expose receiver-less properties through an inherited classifier namespace
        // (for example a foreign superclass's associated declaration). Walk only namespaces whose
        // provider explicitly permits that inheritance and consume the normalized Kotlin property;
        // storage/realization never enters signature solving.
        for owner in self.lexical_classifier_callable_owners(scope) {
            if let Ok(property) = self.with_resolver(scope, |resolver| {
                resolver.accessible_classifier_associated_property(owner, spelling)
            }) {
                return crate::fir::ResolvedTy::new(property.ty)
                    .map_err(|_| Self::failure())
                    .map(SelectedValue::plain);
            }
        }
        // Enum entries are values exported by an explicit member import or a classifier star
        // import. They have no callable/property facet, so the ordinary imported-value resolver
        // cannot select them; retain the already-normalized classifier import rung and select the
        // declaration fact directly. Lexical values and receiver members above remain nearer.
        let imports = self.function_import_scope(scope.source)?;
        if let Some((crate::symbol_source::SymbolNamespace::Classifier(owner), declared_name)) =
            imports.explicit_target(spelling)
        {
            if self.classifier_has_enum_entry(owner, &declared_name) {
                return Self::enum_entry_value(owner, &declared_name);
            }
            // A companion-declared property may be realized by an associated platform
            // accessor rather than an instance accessor (`@JvmField` on JVM). Explicit import
            // still denotes the Kotlin property declaration; consume the provider-normalized
            // property exactly as the ordinary Pass-2 checker does.
            if let Ok(property) = self.with_resolver(scope, |resolver| {
                resolver.associated_property(owner, &declared_name)
            }) {
                return crate::fir::ResolvedTy::new(property.ty)
                    .map_err(|_| Self::failure())
                    .map(SelectedValue::plain);
            }
        }
        let mut imported_entry_owners = Vec::new();
        for owner in imports.levels()[1].iter().copied() {
            if self.classifier_has_enum_entry(owner, spelling)
                && !imported_entry_owners.contains(&owner)
            {
                imported_entry_owners.push(owner);
            }
        }
        match imported_entry_owners.as_slice() {
            [owner] => {
                return Self::enum_entry_value(*owner, spelling);
            }
            [_, _, ..] => return Err(Self::failure()),
            [] => {}
        }
        let property = match self.with_resolver(scope, |resolver| {
            let properties = resolver
                .resolve_symbol(
                    crate::symbol_resolver::SymRecv::TopLevel,
                    spelling,
                    &[],
                    &[],
                )?
                .values();
            let module =
                crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let oracle = crate::symbol_resolver::SourceOracle(&source);
            let receivers = self.implicit_receivers(scope);
            let mut applicable = properties
                .into_iter()
                .filter(|property| {
                    property.kind == crate::libraries::PropKind::TopLevel
                        && property.context_count <= property.getter.params.len()
                        && super::super::context_argument_types(
                            &receivers,
                            &property.getter.params[..property.context_count],
                            &oracle,
                        )
                        .is_some()
                })
                .collect::<Vec<_>>();
            let best_context = applicable
                .iter()
                .map(|property| property.context_count)
                .max()?;
            applicable.retain(|property| property.context_count == best_context);
            let [property] = applicable.as_slice() else {
                return None;
            };
            Some(property.clone())
        }) {
            Ok(property) => property,
            Err(_) => {
                // A PRIMARY CONSTRUCTOR PARAMETER read from a member property initializer
                // (`class A(y: Int) { var x = y }`). It is not a member and not a top-level value, so no
                // ordinary lookup finds it; the enclosing classifier's constructor shape is where it lives.
                if let Some(parameter) =
                    self.demanded_enclosing_constructor_parameter(scope, spelling, demand)?
                {
                    return crate::fir::ResolvedTy::new(parameter)
                        .map(SelectedValue::plain)
                        .map_err(|_| Self::failure());
                }
                // The authoritative classifier operation owns lexical declaration, inherited,
                // and file/import scope priority, including enum-entry-owned nested declarations.
                let classifier = self
                    .qualified_classifier(scope, spelling)
                    .ok_or_else(Self::failure)?;
                let Some(value) = self.classifier_value_type(classifier) else {
                    crate::trace_compiler!(
                        "signature",
                        "select_value declined {spelling}: classifier is neither a singleton nor has a companion",
                    );
                    return Err(Self::failure());
                };
                return crate::fir::ResolvedTy::new(value)
                    .map(SelectedValue::plain)
                    .map_err(|_| Self::failure());
            }
        };
        if let Some(signature) = self.demanded_source_signature_at(
            Some(scope),
            property.stable_declaration,
            Some(origin),
            demand,
        )? {
            return Ok(SelectedValue::plain(signature.result));
        }
        crate::fir::ResolvedTy::new(property.ty)
            .map(SelectedValue::plain)
            .map_err(|_| Self::failure())
    }
}
