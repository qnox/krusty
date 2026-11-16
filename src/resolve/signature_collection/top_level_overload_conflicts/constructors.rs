//! Constructors in a package's overload scope.
//!
//! A class's constructors, and the ones a typealias to a class exposes, are callables named by the
//! class or the alias. kotlinc reports them against same-named functions with the same overload
//! shape: `class C(x: Int)` beside `fun C(x: Int)`, or `typealias CC = C` beside `fun CC(x: Int)`.
//! A constructor no declaration outside its class can call (a private or protected one) still
//! reports the functions it sees, but no function sees it.
//!
//! The constructors are the provider's ordinary constructor candidates; an alias's are its
//! target's, written over the alias's own type parameters as a call through the alias sees them.

use super::*;
use crate::fir::{DeclarationKind, ResolvedModuleIndex, StreamedHeaderModule};
use crate::libraries::{FunctionInfo, GenericSig, LibraryMember};

/// Classify the package's overload conflicts from its top-level functions, given in the order
/// they register, and the constructors its classes and typealiases name, then report them.
pub(in crate::resolve) fn commit_package_overload_conflicts(
    headers: &StreamedHeaderModule,
    index: &ResolvedModuleIndex,
    table: &mut SymbolTable,
    functions: Vec<TopLevelFunctionConflictRegistration>,
    diags: &mut DiagSink,
) {
    let mut groups = TopLevelFunctionConflictGroups::default();
    let mut pending = HashMap::default();
    let mut reserved_diagnostic_bytes = 0usize;
    let mut retained_display_bytes = 0usize;
    let source = TopLevelFunctionConflictDisplaySource::Compact(headers, index);
    // A hidden function is no candidate at all, so it neither reports nor is reported.
    let hidden = |registration: &TopLevelFunctionConflictRegistration| match registration
        .declaration
        .declaration
    {
        TopLevelFunctionConflictDeclaration::Stable(declaration) => index
            .declaration_applied_annotations(declaration)
            .iter()
            .any(crate::types::ResolvedAnnotation::is_deprecated_hidden),
        _ => false,
    };
    for registration in functions
        .into_iter()
        .filter(|registration| !hidden(registration))
    {
        register_top_level_function_conflict(
            source,
            &mut groups,
            registration,
            &mut pending,
            &mut reserved_diagnostic_bytes,
            &mut retained_display_bytes,
        );
    }
    // A constructor that no function sees reads the complete function inventory, so every
    // constructor registers after every function.
    // A constructor conflicts with a function and with another constructor of its own class. Two
    // classifiers of one name are a classifier redeclaration, not an overload conflict.
    let mut constructor_owners = HashMap::<TopLevelFunctionConflictKey, _>::default();
    for (current, key, hidden, display) in package_constructors(headers, index, table) {
        let TopLevelFunctionConflictDeclaration::Constructor(owner, _) = current.declaration else {
            unreachable!("a package constructor names its declaration");
        };
        let functions_share_shape = groups.contains(&key) && !constructor_owners.contains_key(&key);
        if !functions_share_shape
            && *constructor_owners.entry(key.clone()).or_insert(owner) != owner
        {
            continue;
        }
        groups
            .constructor_displays
            .insert(current.declaration, display);
        if hidden {
            register_hidden_top_level_conflict(
                source,
                &mut groups,
                &key,
                current,
                &mut pending,
                &mut reserved_diagnostic_bytes,
                &mut retained_display_bytes,
            );
            continue;
        }
        let private = headers
            .stub(owner)
            .is_some_and(|stub| stub.visibility.is_private());
        register_top_level_function_conflict(
            source,
            &mut groups,
            TopLevelFunctionConflictRegistration {
                key,
                declaration: current,
                private,
                entry_point: false,
            },
            &mut pending,
            &mut reserved_diagnostic_bytes,
            &mut retained_display_bytes,
        );
    }
    commit_top_level_conflict_groups(table, &groups, &pending, reserved_diagnostic_bytes, diags);
}

/// One package constructor: where it is reported, its overload shape, whether no function can
/// see it (it is private or protected), and its display.
type PackageConstructor = (
    TopLevelFunctionConflictDecl,
    TopLevelFunctionConflictKey,
    bool,
    String,
);

/// Every constructor a top-level class or typealias names in its package.
fn package_constructors(
    headers: &StreamedHeaderModule,
    index: &ResolvedModuleIndex,
    table: &SymbolTable,
) -> Vec<PackageConstructor> {
    let mut constructors = Vec::new();
    for stub in headers.stubs.iter().filter(|stub| {
        matches!(
            stub.kind,
            DeclarationKind::Classifier | DeclarationKind::TypeAlias
        )
            // An `expect` classifier's constructors are its `actual`'s, which the `actual`
            // class or typealias brings into the package itself.
            && !stub.flags.has(crate::fir::DeclarationFlags::EXPECT)
            && headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner.is_none())
    }) {
        let Some(name) = stub
            .lookup_name
            .and_then(|name| headers.lookup_names.get(name))
        else {
            continue;
        };
        let Some(package) = headers.sources.get(stub.source).map(|file| file.package) else {
            continue;
        };
        let module = crate::fir::StreamedModuleSymbols::for_file(index, stub.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![&module, &*table.libraries]);
        let declared = match stub.kind {
            DeclarationKind::TypeAlias => alias_constructors(index, &source, stub.id),
            _ => index
                .classifier_identity(stub.id)
                .map(|internal| classifier_constructors(&source, internal))
                .unwrap_or_default(),
        };
        for (ordinal, (signature, member)) in declared.into_iter().enumerate() {
            // A hidden constructor is no candidate at all, so it neither reports nor is reported.
            if member.deprecated_hidden() {
                continue;
            }
            let Some(key) = TopLevelFunctionConflictKey::from_constructor(
                package,
                name.to_owned(),
                &signature,
                &member,
            ) else {
                continue;
            };
            // A class's constructor is reported at its own signature, an alias's at the alias.
            let diagnostic_span = match (stub.kind, member.stable_declaration) {
                (DeclarationKind::Classifier, Some(constructor)) => headers
                    .syntax
                    .declaration(constructor)
                    .and_then(|declaration| declaration.name_span),
                (DeclarationKind::Classifier, None) => None,
                _ => match headers.syntax.declaration(stub.id).map(|alias| alias.kind) {
                    Some(crate::fir::HeaderDeclarationKind::TypeAlias { declaration, .. }) => {
                        Some(declaration)
                    }
                    _ => None,
                },
            };
            let Some(diagnostic_span) = diagnostic_span else {
                continue;
            };
            let current = TopLevelFunctionConflictDecl {
                file: stub.source.raw(),
                declaration: TopLevelFunctionConflictDeclaration::Constructor(
                    stub.id,
                    u32::try_from(ordinal).expect("too many constructors"),
                ),
                diagnostic_span,
            };
            let hidden = matches!(
                member.visibility,
                crate::types::Visibility::Private | crate::types::Visibility::Protected
            );
            let display = Checker::generic_constructor_display(&signature, &member);
            constructors.push((current, key, hidden, display));
        }
    }
    constructors
}

/// A classifier's constructors as the scope's constructor candidates present them.
fn classifier_constructors(
    source: &dyn crate::symbol_source::SymbolSource,
    internal: TypeName,
) -> Vec<(GenericSig, LibraryMember)> {
    let Some(classifier) = source.classifier(internal) else {
        return Vec::new();
    };
    if classifier.outer_instance.is_some() {
        return Vec::new();
    }
    classifier
        .constructors
        .iter()
        .map(|constructor| {
            let candidate = FunctionInfo::scope_constructor(
                internal,
                &classifier,
                constructor.clone(),
                crate::libraries::ConstructorOrigin::Declared,
            );
            let signature = candidate.semantic_signature().into_owned();
            let member = candidate.member_with_return(signature.ret);
            (signature, member)
        })
        .collect()
}

/// The target's constructors over the alias's own type parameters: `typealias GG<X> = G<X>`
/// exposes `constructor<X>(t: X): G<X>` for `class G<T>(t: T)`.
fn alias_constructors(
    index: &ResolvedModuleIndex,
    source: &dyn crate::symbol_source::SymbolSource,
    alias: crate::fir::DeclarationId,
) -> Vec<(GenericSig, LibraryMember)> {
    let Some(header) = index.type_alias_header(alias) else {
        return Vec::new();
    };
    let expansion = header.expansion.get();
    let formals = (0..)
        .map_while(|ordinal| index.type_parameter(alias, ordinal))
        .filter_map(|parameter| index.type_parameter_semantic_name(parameter))
        .map(crate::types::TyParamName::from)
        .collect::<Vec<_>>();
    classifier_constructors(source, header.target)
        .into_iter()
        .map(|(signature, mut member)| {
            let bindings = signature
                .formals
                .iter()
                .copied()
                .zip(expansion.type_args().iter().copied())
                .collect::<crate::symbol_resolver::GSigBinds>();
            let apply = |ty| crate::types::ty_subst_applied_arguments(ty, &bindings);
            let aliased = GenericSig {
                formal_bounds: vec![Vec::new(); formals.len()],
                formals: formals.clone(),
                receiver: None,
                params: signature.params.iter().copied().map(apply).collect(),
                ret: expansion,
                return_policy: signature.return_policy,
            };
            member.params.clone_from(&aliased.params);
            member.ret = expansion;
            (aliased, member)
        })
        .collect()
}
