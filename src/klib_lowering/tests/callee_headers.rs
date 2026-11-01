//! Callees declared from their serialized headers: a dependency body's callee that no checked call
//! selected is described by its own KLIB IR declaration, and a provider record of the same
//! identity, when there is one, must agree with that header.

use super::super::*;
use super::body_forms::{
    identity_and_caller, kotlin, IDENTITY_AND_CALLER, IDENTITY_AND_CALLER_SOURCE,
};
use super::demo_package::{
    assert_unit_is_empty, demo, demo_with, operator, source_body, Body, Demo, Shape, T,
};
use super::{base, local_symbol, lowered_body, public_symbol, validated};
use crate::fir::ResolvedParameterIdentity;
use crate::libraries::{
    ExternalCallableKind, GenericSig, KlibDeclarationSignature, KlibDependencyCallable,
    SingletonDispatch,
};
use crate::metadata::id_signature::{
    callable_signature, metadata_class_signature, CallableShape, DeclarationContainer,
    KlibPublicIdSignature, MetadataClass, MetadataContainer, Placement,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrBody, KlibIrExprKind, KlibIrFunction, KlibIrMember, KlibIrNullability,
    KlibIrParameter, KlibIrStatement, KlibIrType, KlibIrTypeParameter,
};
use crate::metadata::klib_ir::{KlibIrDeclarationTree, KlibIrSymbolKind};
use crate::metadata::semantic::{KotlinType, KotlinTypeParameter, KotlinTypeParameterId};
use crate::types::Ty;

/// Every function of a unit as a backend reads its declaration.
fn declarations(unit: &DependencyBodyUnit) -> Vec<String> {
    let ir = unit.ir();
    ir.functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            let id = crate::ir::FunId::try_from(index).expect("fits");
            format!(
                "{} {:?} -> {:?} static={} dispatch={:?} identities={:?} extension={} contexts={:?}",
                function.name,
                function.params,
                function.ret,
                function.is_static,
                function.dispatch_receiver,
                ir.fn_params[&id].identities,
                ir.extension_receiver_fns.contains(&id),
                ir.fn_context_counts.get(&id),
            )
        })
        .collect()
}

#[test]
fn a_callee_no_checked_call_selected_is_declared_from_its_header() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut from_header = DependencyBodyUnit::default();
    let a = demo
        .lower(&mut from_header, "a", &[])
        .expect("the callee's header describes it");
    validated(from_header.ir());
    assert_eq!(
        lowered_body(&from_header, a),
        source_body(IDENTITY_AND_CALLER_SOURCE, "a", 1)
    );
    // The header declares exactly what the provider record of the same identity does.
    let mut from_record = DependencyBodyUnit::default();
    let selected = demo.lower(&mut from_record, "a", &["b"]).expect("lowers");
    assert_eq!(
        lowered_body(&from_header, a),
        lowered_body(&from_record, selected)
    );
    assert_eq!(declarations(&from_header), declarations(&from_record));
    assert_eq!(
        declarations(&from_header),
        [
            "a [Obj(TypeName(\"kotlin/Int\"), [])] -> Obj(TypeName(\"kotlin/Int\"), []) static=true \
             dispatch=None identities=[IrParameterIdentity { source_name: Some(\"a\"), role: Value, \
             provenance: SourceDeclared }] extension=false contexts=None",
            "b [Obj(TypeName(\"kotlin/Int\"), [])] -> Obj(TypeName(\"kotlin/Int\"), []) static=true \
             dispatch=None identities=[IrParameterIdentity { source_name: Some(\"a\"), role: Value, \
             provenance: SourceDeclared }] extension=false contexts=None",
        ]
    );
}

#[test]
fn an_unauthenticated_header_declared_callee_without_a_body_declines() {
    let demo = demo(&IDENTITY_AND_CALLER, |name, body| match name {
        "a" => identity_and_caller(name, body),
        _ => None,
    });
    assert_eq!(
        demo.declined("a", &[]),
        "the KLIB body of `demo.a` (it reaches the KLIB body of `demo.b` \
         (its library serializes none))"
    );
}

#[test]
fn a_serialized_function_must_agree_with_frozen_declaration_modifiers() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let selected = demo.facts["b"]
        .klib_body_callable()
        .expect("a frozen KLIB declaration");
    let tree = function_tree(demo_signature(None, "b", &[], &[T::Int]), "b", false, 0, 1);
    let KlibIrMember::Function(id) = tree.declaration else {
        panic!("a function")
    };
    let function = tree.arena.function(id);
    super::super::function_lowering::function_header(Some(selected), &tree.arena, function)
        .unwrap_or_else(|reason| panic!("the original declaration agrees: {reason:?}"));
    // Serialized FunctionFlags, not the shorter metadata accessor flag word.
    for flag in [1 << 10, 1 << 12, 1 << 13, 1 << 5] {
        let mut changed = function.clone();
        changed.base.flags |= flag;
        let Err(reason) =
            super::super::function_lowering::function_header(Some(selected), &tree.arena, &changed)
        else {
            panic!("changed declaration modifier {flag} must not authenticate")
        };
        assert_eq!(
            reason,
            KlibBodyDeclineReason::SignatureMismatch(
                "the serialized function modifiers disagree with its selected declaration"
                    .to_owned(),
            )
        );
    }
}

#[test]
fn an_external_declaration_retains_dispatch_without_authorizing_body_emission() {
    use super::super::function_lowering::{raw_function_header, HeaderPurpose};
    use crate::types::type_name;
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut fact = demo.facts["b"].clone();
    let owner = type_name("demo/Box");
    let signature = demo_signature(Some("Box"), "b", &[], &[T::Int]);
    fact.kind = ExternalCallableKind::Member;
    fact.declaration_owner = Some(crate::types::SemanticCallableOwner::Classifier(owner));
    fact.declaration_signature = Some(KlibDeclarationSignature::Public(signature.clone()));
    fact.dependency_callable
        .as_mut()
        .expect("module owned")
        .declaration = KlibDeclarationSignature::Public(signature.clone());
    let tree = function_tree(signature, "b", true, 0, 1);
    let KlibIrMember::Function(id) = tree.declaration else {
        panic!("a function")
    };
    let mut function = tree.arena.function(id).clone();
    function.body = None;
    let selected = fact.klib_body_callable().expect("a selected member");
    let header = raw_function_header(
        Some(selected),
        HeaderPurpose::External,
        &tree.arena,
        &function,
    )
    .expect("an external declaration keeps its receiver rather than erasing it");
    let (parameters, result, contexts, extension, dispatch) = header.linked_signature();
    assert_eq!(parameters, &[Ty::Int]);
    assert_eq!(result, Ty::Int);
    assert_eq!(contexts, 0);
    assert!(!extension);
    assert_eq!(dispatch, Some(Ty::obj_name(owner)));
    assert_eq!(
        super::super::function_lowering::function_header(Some(selected), &tree.arena, &function)
            .err()
            .expect("external authentication is not permission to erase dispatch"),
        KlibBodyDeclineReason::SignatureMismatch(
            "the serialized dispatch receiver disagrees with the selected or emitted singleton"
                .to_owned()
        )
    );
    fact.declaration_owner = Some(crate::types::SemanticCallableOwner::Classifier(type_name(
        "demo/Other",
    )));
    assert_eq!(
        raw_function_header(
            fact.klib_body_callable(),
            HeaderPurpose::External,
            &tree.arena,
            &function
        )
        .err()
        .expect("a foreign declaration owner cannot authenticate the dispatch"),
        KlibBodyDeclineReason::SignatureMismatch(
            "the external dispatch receiver disagrees with its selected declaration owner"
                .to_owned()
        )
    );
}

#[test]
fn matching_function_modifiers_survive_the_provider_and_frozen_handoff() {
    use crate::metadata::semantic::{KotlinModality, KotlinPackage};
    for (inline, suspend, external, modality, flags) in [
        (true, false, false, KotlinModality::Final, 1 << 10),
        (false, true, false, KotlinModality::Final, 1 << 13),
        (false, false, true, KotlinModality::Final, 1 << 12),
        (false, false, false, KotlinModality::Open, 1 << 4),
        (false, false, false, KotlinModality::Abstract, 1 << 5),
    ] {
        let mut declaration = super::demo_package::declaration(("b", &[T::Int], T::Int));
        declaration.is_inline = inline;
        declaration.is_suspend = suspend;
        declaration.is_external = external;
        declaration.modality = modality;
        let libraries = crate::klib_libraries::KlibLibraries::from_named_packages(
            "demo",
            vec![(
                vec!["demo".to_owned()],
                KotlinPackage {
                    functions: vec![declaration],
                    ..Default::default()
                },
            )],
        )
        .expect("a declaration-owned modifier contract");
        let selected = super::frozen_function(&libraries, "demo", "b", None, &[T::Int.semantic()]);
        let contract = selected
            .klib_body_declaration_modifiers
            .expect("known metadata modifiers");
        assert_eq!(contract.is_inline, inline);
        assert_eq!(contract.is_suspend, suspend);
        assert_eq!(contract.is_external, external);
        assert!(!contract.property_is_external);
        assert_eq!(contract.modality, modality);
        let Some(KlibDeclarationSignature::Public(signature)) = &selected.declaration_signature
        else {
            panic!("a function signature")
        };
        let tree = function_tree(signature.clone(), "b", false, 0, 1);
        let KlibIrMember::Function(id) = tree.declaration else {
            panic!("a function")
        };
        let mut function = tree.arena.function(id).clone();
        function.base.flags = flags;
        super::super::function_lowering::function_header(
            selected.klib_body_callable(),
            &tree.arena,
            &function,
        )
        .unwrap_or_else(|reason| panic!("matching metadata and IR modifiers: {reason:?}"));
    }
}

#[test]
fn dispatch_slots_cannot_carry_a_vararg_marker() {
    use super::super::function_lowering::{
        prepared_inline_header, raw_function_header, HeaderPurpose,
    };
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut fact = demo.facts["b"].clone();
    let owner = crate::types::type_name("demo/Box");
    let signature = demo_signature(Some("Box"), "b", &[], &[T::Int]);
    fact.kind = ExternalCallableKind::Member;
    fact.declaration_owner = Some(crate::types::SemanticCallableOwner::Classifier(owner));
    fact.declaration_signature = Some(KlibDeclarationSignature::Public(signature.clone()));
    fact.dependency_callable.as_mut().unwrap().declaration =
        KlibDeclarationSignature::Public(signature.clone());
    let tree = function_tree(signature, "b", true, 0, 1);
    let KlibIrMember::Function(id) = tree.declaration else {
        panic!("function")
    };
    let mut function = tree.arena.function(id).clone();
    let selected = fact.klib_body_callable().unwrap();
    raw_function_header(
        Some(selected),
        HeaderPurpose::External,
        &tree.arena,
        &function,
    )
    .unwrap();
    prepared_inline_header(selected, &[], &tree.arena, &function).unwrap();
    function
        .dispatch_receiver
        .as_mut()
        .unwrap()
        .vararg_element_type = Some(function.return_type);
    let expected = Some(KlibBodyDeclineReason::SignatureMismatch(
        "a serialized dispatch receiver carries a vararg marker".into(),
    ));
    assert_eq!(
        raw_function_header(
            Some(selected),
            HeaderPurpose::External,
            &tree.arena,
            &function
        )
        .err(),
        expected
    );
    assert_eq!(
        prepared_inline_header(selected, &[], &tree.arena, &function).err(),
        expected
    );
    let mut flattened = function;
    let mut receiver = flattened.dispatch_receiver.take().unwrap();
    receiver.base.symbol.kind = KlibIrSymbolKind::ValueParameter;
    flattened.regular_parameters.insert(0, receiver);
    assert_eq!(
        prepared_inline_header(selected, &[], &tree.arena, &flattened).err(),
        expected
    );
}

#[test]
fn missing_or_conflicting_modifier_facts_do_not_coalesce_selected_declarations() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let original = &demo.facts["b"];
    type Change = fn(&mut Option<crate::libraries::KlibBodyDeclarationModifiers>);
    let changes: &[Change] = &[
        |fact| fact.as_mut().expect("known").is_inline = true,
        |fact| fact.as_mut().expect("known").is_suspend = true,
        |fact| fact.as_mut().expect("known").is_external = true,
        |fact| fact.as_mut().expect("known").property_is_external = true,
        |fact| {
            fact.as_mut().expect("known").modality = crate::metadata::semantic::KotlinModality::Open
        },
        |fact| *fact = None,
    ];
    for change in changes {
        let mut changed = original.clone();
        change(&mut changed.klib_body_declaration_modifiers);
        let first = original.klib_body_callable().expect("selected");
        let second = changed.klib_body_callable().expect("same exact identity");
        assert!(!first.same_declaration_contract(second));
        let facts = KlibCalleeFacts::new([first, second]);
        assert!(matches!(
            facts.get(first.identity()),
            Some(super::super::callee_facts::KlibCalleeFact::Ambiguous)
        ));
    }
}

#[test]
fn provider_header_evidence_does_not_invent_a_checked_call_obligation() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let root = demo.facts["a"].klib_body_callable().expect("selected root");
    let declaration = demo.facts["b"]
        .klib_body_callable()
        .expect("provider declaration");
    let facts = KlibCalleeFacts::with_declarations([root], [declaration]);
    assert!(facts.is_selected(root.identity()));
    assert!(!facts.is_selected(declaration.identity()));
    let Some(super::super::callee_facts::KlibCalleeFact::Selected(headers)) =
        facts.get(declaration.identity())
    else {
        panic!("unselected provider evidence still authenticates the header");
    };
    assert_eq!(headers.len(), 1);
    assert!(headers[0].same_declaration_contract(declaration));
    let mut conflicting = demo.facts["b"].clone();
    conflicting
        .klib_body_declaration_modifiers
        .as_mut()
        .expect("known")
        .is_inline = true;
    let facts = KlibCalleeFacts::with_declarations(
        [root],
        [
            declaration,
            conflicting.klib_body_callable().expect("same identity"),
        ],
    );
    assert!(!facts.is_selected(declaration.identity()));
    assert!(matches!(
        facts.get(declaration.identity()),
        Some(super::super::callee_facts::KlibCalleeFact::Ambiguous)
    ));
}

#[test]
fn common_builtin_capability_distinguishes_headers_source_calls_and_conflicts() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut provider = demo.facts["b"].clone();
    let signature = operator(&["kotlin"], Some("Boolean"), "not", &[]).0;
    provider.dependency_callable = Some(KlibDependencyCallable {
        module: "kotlin".to_owned(),
        declaration: KlibDeclarationSignature::Public(signature),
    });
    let header = provider.klib_body_callable().expect("provider header");
    let unit = DependencyBodyUnit::default();
    assert!(unit.folds_common_builtin(header.identity(), &KlibCalleeFacts::default()));
    let headers = KlibCalleeFacts::with_declarations([], [header]);
    assert!(unit.folds_common_builtin(header.identity(), &headers));
    // Roots enter as source selections, so a recursive builtin edge is not folded either.
    let selected = KlibCalleeFacts::new([header]);
    assert!(!unit.folds_common_builtin(header.identity(), &selected));
    let mut conflicting = provider.clone();
    conflicting
        .klib_body_declaration_modifiers
        .as_mut()
        .expect("known declaration")
        .is_inline = true;
    let headers = KlibCalleeFacts::with_declarations(
        [],
        [
            header,
            conflicting.klib_body_callable().expect("same identity"),
        ],
    );
    assert!(!unit.folds_common_builtin(header.identity(), &headers));
    assert!(!unit.folds_common_builtin(
        demo.facts["b"]
            .klib_body_callable()
            .expect("ordinary")
            .identity(),
        &KlibCalleeFacts::default(),
    ));
}

#[test]
fn a_previously_retained_builtin_edge_is_not_folded_without_a_current_source_call() {
    let signature = operator(&["kotlin"], Some("Boolean"), "not", &[]).0;
    let mut arena = KlibIrArena::default();
    let boolean = arena.push_type(T::Boolean.klib());
    let declaration = arena.push_function(KlibIrFunction {
        base: base(public_symbol(KlibIrSymbolKind::Function, signature.clone())),
        name: "not".to_owned(),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver: Some(KlibIrParameter {
            base: base(local_symbol(KlibIrSymbolKind::ReceiverParameter, 0)),
            name: "<this>".to_owned(),
            ty: boolean,
            vararg_element_type: None,
            default_value: None,
        }),
        context_parameters: Vec::new(),
        extension_receiver: None,
        regular_parameters: Vec::new(),
        return_type: boolean,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: None,
    });
    let demo = demo_with(
        &[("f", &[T::Boolean], T::Boolean)],
        vec![KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Function(declaration),
        }],
        |_, body| {
            let operand = body.param(0);
            let call = body.call_signature(signature.clone(), vec![operand], None, T::Boolean);
            Some(vec![body.ret(call)])
        },
    );
    let external = crate::fir::ExternalCallableId::from_raw(27);
    let identity = KlibDependencyCallable {
        module: "demo".to_owned(),
        declaration: KlibDeclarationSignature::Public(signature),
    };
    let emitted = [(
        identity,
        super::super::EmittedDependencyCallable::new(external, None, None),
    )]
    .into_iter()
    .collect();
    let callees = demo.callees(&["f"]);
    let mut unit = DependencyBodyUnit::default();
    unit.lower_function_with_intrinsics_and_externals(
        demo.callable("f"),
        &demo.bodies,
        &callees,
        crate::klib_libraries::KlibIntrinsicFacts::empty(),
        &emitted,
    )
    .expect("the retained exact edge remains external");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 1);
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            crate::ir::IrExpr::Call { callee, .. } => Some(&**callee),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1, "the retained call is not a folded negation");
    assert!(matches!(calls[0], crate::ir::Callee::External { target, .. } if *target == external));
}

#[test]
fn provider_header_evidence_does_not_invent_a_call_edge_variant() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut provider = demo.facts["b"].clone();
    provider.kind = ExternalCallableKind::Member;
    let mut checked = provider.clone();
    checked.singleton_dispatch = Some(crate::libraries::SingletonDispatch {
        classifier: crate::types::type_name("demo/SelectedObject"),
    });
    let declaration = provider.klib_body_callable().expect("provider header");
    let edge = checked.klib_body_callable().expect("checked edge");
    let facts = KlibCalleeFacts::with_declarations([edge], [declaration]);
    let Some(super::super::callee_facts::KlibCalleeFact::Selected(headers)) =
        facts.get(edge.identity())
    else {
        panic!("one declaration contract");
    };
    assert!(facts.is_selected(edge.identity()));
    assert_eq!(
        headers.len(),
        1,
        "the provider record is not another checked edge"
    );
    assert_eq!(headers[0].singleton_dispatch(), edge.singleton_dispatch());
}

/// A provider record of `demo.b(a: Int): Int` with one fact changed.
struct Record {
    name: &'static str,
    params: [Ty; 1],
    identities: [ResolvedParameterIdentity; 1],
    ret: Ty,
    identity: KlibDependencyCallable,
    generic: Option<GenericSig>,
    formals: Vec<Ty>,
    reified: Vec<u32>,
}

impl Record {
    fn of_b(demo: &Demo) -> Self {
        Self {
            name: "b",
            params: [T::Int.semantic()],
            identities: [ResolvedParameterIdentity::Source("a".into())],
            ret: T::Int.semantic(),
            identity: demo.facts["b"]
                .dependency_callable
                .clone()
                .expect("a signed dependency callable"),
            generic: None,
            formals: Vec::new(),
            reified: Vec::new(),
        }
    }

    fn callable(&self) -> KlibCallable<'_> {
        KlibCallable::new(
            crate::libraries::KlibBodyCallableShape {
                class_contract: None,
                accessor: None,
                name: self.name,
                kind: ExternalCallableKind::TopLevel,
                visibility: crate::types::Visibility::Public,
                semantic_role: None,
                params: &self.params,
                ret: self.ret,
                parameter_identities: &self.identities,
                context_count: 0,
                vararg_index: None,
                parameter_defaults: None,
                generic_signature: self.generic.as_ref(),
                type_parameters: &self.formals,
                reified_type_parameter_ordinals: &self.reified,
                compiler_intrinsic: None,
                range_construction: None,
                declaration_modifiers: None,
            },
            None,
            None,
            None,
            Some(&self.identity),
        )
        .expect("signed")
    }
}

/// A raw, unprepared identity body binds a producer symbol to a declaration-owned formal,
/// even when metadata's interned formal identity differs from the producer's source spelling.
fn generic_identity_header() -> (KlibIrArena, KlibIrFunction, Record) {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut record = Record::of_b(&demo);
    let tree = function_tree(demo_signature(None, "b", &[], &[T::Int]), "b", false, 0, 1);
    let KlibIrMember::Function(function) = tree.declaration else {
        panic!("a function tree");
    };
    let mut function = tree.arena.function(function).clone();
    let mut arena = tree.arena;
    let bound = arena.push_type(T::NullableAny.klib());
    let formal = local_symbol(KlibIrSymbolKind::TypeParameter, 80);
    let ty = arena.push_type(KlibIrType::Simple {
        classifier: formal.clone(),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    function.type_parameters.push(KlibIrTypeParameter {
        base: crate::metadata::klib_ir::tree::KlibIrDeclarationBase {
            flags: 4, // Serialized IrFlags: invariant, non-reified.
            ..base(formal)
        },
        name: "T".to_owned(),
        supertypes: vec![bound],
    });
    function.regular_parameters[0].ty = ty;
    function.return_type = ty;
    let semantic = Ty::ty_param("declaration-owned-formal", T::NullableAny.semantic());
    record.params = [semantic];
    record.formals = vec![semantic];
    record.ret = semantic;
    record.generic = Some(GenericSig {
        formals: vec!["declaration-owned-formal".to_owned()],
        formal_bounds: vec![Vec::new()],
        receiver: None,
        params: vec![semantic],
        ret: semantic,
        return_policy: Default::default(),
    });
    (arena, function, record)
}

#[test]
fn an_ordinary_generic_header_preserves_the_declared_formal_not_its_erasure() {
    let (arena, function, record) = generic_identity_header();
    let header = super::super::function_lowering::function_header(
        Some(record.callable()),
        &arena,
        &function,
    )
    .unwrap_or_else(|reason| {
        panic!("an ordinary generic header must bind its formals: {reason:?}")
    });
    assert_eq!(header.params(), record.params);
    assert_eq!(header.result(), record.ret);
    assert_eq!(header.body_result(), record.ret);
    assert_eq!(header.erased(record.ret), record.ret);
}

#[test]
fn an_ordinary_generic_header_checks_every_declared_upper_bound() {
    let (mut arena, mut function, mut record) = generic_identity_header();
    let string = arena.push_type(T::String.klib());
    function.type_parameters[0].supertypes.push(string);
    let generic = record.generic.as_mut().expect("generic");
    generic.formal_bounds = vec![vec![T::NullableAny.semantic(), T::String.semantic()]];
    super::super::function_lowering::function_header(Some(record.callable()), &arena, &function)
        .unwrap_or_else(|reason| panic!("matching complete bounds: {reason:?}"));
    record.generic.as_mut().expect("generic").formal_bounds[0][1] = T::Int.semantic();
    let result = super::super::function_lowering::function_header(
        Some(record.callable()),
        &arena,
        &function,
    );
    assert_eq!(
        result.err(),
        Some(KlibBodyDeclineReason::SignatureMismatch(
            "type parameter 0 has different serialized and selected upper bounds".to_owned(),
        ))
    );
}

#[test]
fn a_raw_generic_header_does_not_accept_a_prepared_erased_parameter() {
    let (mut arena, mut function, record) = generic_identity_header();
    function.regular_parameters[0].ty = arena.push_type(T::NullableAny.klib());
    assert_eq!(
        super::super::function_lowering::function_header(
            Some(record.callable()),
            &arena,
            &function
        )
        .err(),
        Some(KlibBodyDeclineReason::SignatureMismatch(format!(
            "parameter 0 is {:?} serialized and {:?} selected",
            T::NullableAny.semantic(),
            record.params[0],
        )))
    );
}

#[test]
fn reified_dependency_bodies_cannot_be_emitted_as_ordinary_generic_functions() {
    let (arena, mut function, mut record) = generic_identity_header();
    function.type_parameters[0].base.flags |= 8;
    record.reified.push(0);
    assert_eq!(
        super::super::function_lowering::function_header(
            Some(record.callable()),
            &arena,
            &function
        )
        .err(),
        Some(KlibBodyDeclineReason::UnsupportedDeclaration(
            "reified type parameters"
        ))
    );
}

#[test]
fn selected_callee_contracts_include_all_generic_bounds_and_reification() {
    let (_, _, record) = generic_identity_header();
    let mut other = Record {
        generic: record.generic.clone(),
        ..Record::of_b(&demo(&IDENTITY_AND_CALLER, identity_and_caller))
    };
    other.params = record.params;
    other.ret = record.ret;
    other.formals = record.formals.clone();
    assert!(record
        .callable()
        .same_declaration_contract(other.callable()));
    other.reified.push(0);
    assert!(!record
        .callable()
        .same_declaration_contract(other.callable()));
    other.reified.clear();
    other.generic.as_mut().expect("generic").formal_bounds[0].push(T::String.semantic());
    assert!(!record
        .callable()
        .same_declaration_contract(other.callable()));
}

fn metadata_formal(name: &str, id: u64, bounds: Vec<KotlinType>) -> KotlinTypeParameter {
    KotlinTypeParameter {
        id: KotlinTypeParameterId(id),
        name: name.to_owned(),
        bounds,
        variance: crate::types::TypeVariance::Invariant,
        only_input: false,
        reified: false,
    }
}

fn metadata_parameter(name: &str, id: u64) -> KotlinType {
    KotlinType::Param {
        name: name.to_owned(),
        id: KotlinTypeParameterId(id),
        nullable: false,
    }
}

/// Go through the real signed provider and frozen handoff, not a hand-built GenericSig: its full
/// bounds and its parameter types have intentionally different finite self-reference depths.
fn metadata_generic_record(formals: Vec<KotlinTypeParameter>) -> Record {
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    let mut function = super::coerce_declaration();
    function.name = "b".to_owned();
    function.receiver = None;
    function.params = vec![metadata_parameter("T", 0)];
    function.param_names = vec!["a".to_owned()];
    function.ret = metadata_parameter("T", 0);
    function.formals = formals;
    let libraries = crate::klib_libraries::KlibLibraries::from_named_packages(
        "demo",
        vec![(
            vec!["demo".to_owned()],
            crate::metadata::semantic::KotlinPackage {
                functions: vec![function],
                ..Default::default()
            },
        )],
    )
    .expect("a signed generic declaration");
    let symbols = libraries.symbols(
        SymbolNamespace::Package(crate::types::type_name("demo")),
        "b",
    );
    let [selected] = symbols.callables.functions() else {
        panic!("one generic function");
    };
    let fact = super::frozen_function(&libraries, "demo", "b", None, &selected.callable.params);
    Record {
        name: "b",
        params: [fact.params[0]],
        ret: fact.ret,
        identities: [ResolvedParameterIdentity::Source("a".into())],
        identity: fact.dependency_callable.expect("named dependency"),
        generic: fact.generic_sig.map(|signature| *signature),
        formals: fact.klib_body_type_parameters.into_vec(),
        reified: Vec::new(),
    }
}

#[test]
fn recursive_formals_keep_the_real_provider_normalization_through_the_frozen_handoff() {
    let recursive = KotlinType::Class {
        internal: "kotlin/Comparable".to_owned(),
        args: vec![metadata_parameter("T", 0)],
        nullable: false,
        shape: Default::default(),
    };
    let record = metadata_generic_record(vec![metadata_formal("T", 0, vec![recursive])]);
    let (mut arena, mut function, _) = generic_identity_header();
    let formal = function.type_parameters[0].base.symbol.clone();
    let ty = arena.push_type(KlibIrType::Simple {
        classifier: formal,
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    let mut bound = T::String.klib();
    let KlibIrType::Simple {
        classifier,
        arguments,
        ..
    } = &mut bound
    else {
        panic!("a class type");
    };
    *classifier = public_symbol(
        KlibIrSymbolKind::Class,
        metadata_class_signature(
            MetadataContainer {
                package: &["kotlin".to_owned()],
                classes: &[],
                native_interop_library: false,
            },
            MetadataClass {
                name: "Comparable",
                type_params: &[],
                expect: false,
            },
        ),
    );
    arguments.push(crate::metadata::klib_ir::tree::KlibIrTypeArgument::Type {
        variance: crate::metadata::klib_ir::tree::KlibIrVariance::Invariant,
        ty,
    });
    function.type_parameters[0].supertypes = vec![arena.push_type(bound)];
    let header = super::super::function_lowering::function_header(
        Some(record.callable()),
        &arena,
        &function,
    )
    .unwrap_or_else(|reason| panic!("real recursive formal: {reason:?}"));
    assert_eq!(header.params(), record.params);
    assert_eq!(header.result(), record.ret);
}

#[test]
fn a_forward_bound_binds_all_real_provider_formals_before_reading_any_bounds() {
    let record = metadata_generic_record(vec![
        metadata_formal("T", 0, vec![metadata_parameter("U", 1)]),
        metadata_formal("U", 1, vec![T::String.kotlin()]),
    ]);
    let (mut arena, mut function, _) = generic_identity_header();
    let symbol = local_symbol(KlibIrSymbolKind::TypeParameter, 81);
    let ty = arena.push_type(KlibIrType::Simple {
        classifier: symbol.clone(),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    function.type_parameters[0].supertypes = vec![ty];
    function.type_parameters.push(KlibIrTypeParameter {
        base: crate::metadata::klib_ir::tree::KlibIrDeclarationBase {
            flags: 4,
            ..base(symbol)
        },
        name: "U".to_owned(),
        supertypes: vec![arena.push_type(T::String.klib())],
    });
    let header = super::super::function_lowering::function_header(
        Some(record.callable()),
        &arena,
        &function,
    )
    .unwrap_or_else(|reason| panic!("real forward-bound formals: {reason:?}"));
    assert_eq!(header.params(), record.params);
    assert_eq!(header.result(), record.ret);
}

#[test]
fn generic_header_contract_mismatches_are_rejected_without_erasure_or_role_recovery() {
    type Mutation = fn(&mut KlibIrFunction, &mut Record);
    let cases: &[(&str, Mutation)] = &[
        ("0 serialized type parameters, 1 selected", |function, _| {
            function.type_parameters.clear()
        }),
        (
            "the selected formal types are not parallel to its generic signature",
            |_, record| record.formals.clear(),
        ),
        (
            "the selected generic bounds are not parallel to its formals",
            |_, record| record.generic.as_mut().unwrap().formal_bounds.clear(),
        ),
        (
            "the selected generic bounds are not parallel to its formals",
            |_, record| {
                record
                    .generic
                    .as_mut()
                    .unwrap()
                    .formal_bounds
                    .push(Vec::new())
            },
        ),
        (
            "the selected generic signature disagrees with its callable parameters or result",
            |_, record| record.generic.as_mut().unwrap().ret = Ty::String,
        ),
        (
            "the selected generic receiver disagrees with its callable parameter roles",
            |_, record| {
                let signature = record.generic.as_mut().unwrap();
                signature.receiver = signature.params.pop();
            },
        ),
        (
            "type parameter 0 has different serialized and selected reification",
            |function, _| function.type_parameters[0].base.flags |= 8,
        ),
        (
            "type parameter 0 has different serialized and selected reification",
            |_, record| record.reified.push(0),
        ),
        (
            "the selected reified type-parameter ordinals are invalid",
            |_, record| record.reified.push(1),
        ),
        (
            "the selected reified type-parameter ordinals are invalid",
            |_, record| record.reified.extend([0, 0]),
        ),
        (
            "function type parameter 0 is not invariant",
            |function, _| function.type_parameters[0].base.flags = 2,
        ),
        (
            "function type parameter 0 is not invariant",
            |function, _| function.type_parameters[0].base.flags = 6,
        ),
        (
            "type parameter 0 has a different selected formal identity",
            |_, record| record.formals[0] = Ty::String,
        ),
    ];
    for &(message, mutate) in cases {
        let (arena, mut function, mut record) = generic_identity_header();
        mutate(&mut function, &mut record);
        assert_eq!(
            super::super::function_lowering::function_header(
                Some(record.callable()),
                &arena,
                &function
            )
            .err(),
            Some(KlibBodyDeclineReason::SignatureMismatch(message.to_owned())),
            "{message}",
        );
    }
}

#[test]
fn generic_headers_require_distinct_serialized_symbols_and_provider_formal_identities() {
    for duplicate_provider in [false, true] {
        let (arena, mut function, mut record) = generic_identity_header();
        let mut second = function.type_parameters[0].clone();
        second.name = "U".to_owned();
        if duplicate_provider {
            second.base.symbol = local_symbol(KlibIrSymbolKind::TypeParameter, 81);
        }
        function.type_parameters.push(second);
        let name = if duplicate_provider {
            "declaration-owned-formal"
        } else {
            "second-owned-formal"
        };
        let signature = record.generic.as_mut().unwrap();
        signature.formals.push(name.to_owned());
        signature.formal_bounds.push(Vec::new());
        record
            .formals
            .push(Ty::ty_param(name, T::NullableAny.semantic()));
        assert_eq!(
            super::super::function_lowering::function_header(
                Some(record.callable()),
                &arena,
                &function
            )
            .err(),
            Some(KlibBodyDeclineReason::SignatureMismatch(
                "type parameter 1 has no distinct serialized and selected identity".to_owned(),
            )),
        );
    }
}

#[test]
fn a_selected_callee_that_disagrees_with_its_header_declines() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut cases = Vec::new();
    let mut record = Record::of_b(&demo);
    record.params = [T::Double.semantic()];
    cases.push((record, "parameter 0 is Obj(TypeName(\"kotlin/Int\"), []) serialized and Obj(TypeName(\"kotlin/Double\"), []) selected"));
    let mut record = Record::of_b(&demo);
    record.identities = [ResolvedParameterIdentity::Source("z".into())];
    cases.push((
        record,
        "parameter 0 is Source(\"a\") serialized and Source(\"z\") selected",
    ));
    let mut record = Record::of_b(&demo);
    record.name = "bee";
    cases.push((
        record,
        "the declaration is `b` serialized and `bee` selected",
    ));
    let mut record = Record::of_b(&demo);
    record.ret = T::Double.semantic();
    cases.push((record, "the result is Obj(TypeName(\"kotlin/Int\"), []) serialized and Obj(TypeName(\"kotlin/Double\"), []) selected"));
    for (record, detail) in &cases {
        let callees = KlibCalleeFacts::new([record.callable()]);
        let mut unit = DependencyBodyUnit::default();
        let decline = unit
            .lower_function(demo.callable("a"), &demo.bodies, &callees)
            .expect_err("the two views disagree");
        assert_unit_is_empty(&unit);
        assert_eq!(
            decline.to_string(),
            format!(
                "the KLIB body of `demo.a` (it reaches the KLIB body of `demo.b` \
                 (its serialized declaration disagrees with the selected one: {detail}))"
            )
        );
    }
}

#[test]
fn a_header_declared_function_is_cross_checked_when_selected_later() {
    let demo = demo(&IDENTITY_AND_CALLER, identity_and_caller);
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "a", &[]).expect("lowers");
    let functions = unit.ir().functions.len();
    let expressions = unit.ir().exprs.len();
    let mut record = Record::of_b(&demo);
    record.params = [T::Double.semantic()];
    assert_eq!(
        unit.lower_function(record.callable(), &demo.bodies, &KlibCalleeFacts::default())
            .expect_err("the record disagrees with the declared header")
            .to_string(),
        "the KLIB body of `demo.b` (its serialized declaration disagrees with the selected one: \
         parameter 0 is Obj(TypeName(\"kotlin/Int\"), []) serialized and Obj(TypeName(\"kotlin/Double\"), []) selected)"
    );
    let b = demo.lower(&mut unit, "b", &[]).expect("the record agrees");
    assert_eq!(unit.ir().functions.len(), functions);
    assert_eq!(unit.ir().exprs.len(), expressions);
    assert_eq!(unit.ir().fn_source_names[&b], "b");
    validated(unit.ir());
}

/// The signature of `demo.<name>` (or of `demo.<class>.<name>`) with these context and value
/// parameters.
fn demo_signature(
    class: Option<&str>,
    name: &str,
    contexts: &[T],
    values: &[T],
) -> KlibPublicIdSignature {
    let contexts = contexts.iter().map(|ty| ty.kotlin()).collect::<Vec<_>>();
    let values = values.iter().map(|ty| ty.kotlin()).collect::<Vec<_>>();
    if contexts.is_empty() {
        return operator(&["demo"], class, name, &values).0;
    }
    let package = ["demo".to_owned()];
    callable_signature(
        DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &[],
            native_interop_library: false,
        },
        &CallableShape {
            name,
            contexts: contexts.iter().collect(),
            receiver: None,
            params: values.iter().collect(),
            vararg: None,
            type_parameters: Vec::new(),
            expect: false,
            placement: Placement::Ordinary,
        },
    )
    .expect("a non-generic shape is signable")
}

/// A serialized function with a dispatch receiver when `dispatch`, these context and value
/// parameters, all `Int`, whose body returns its first context or value parameter.
fn function_tree(
    signature: KlibPublicIdSignature,
    name: &str,
    dispatch: bool,
    contexts: usize,
    values: usize,
) -> KlibIrDeclarationTree {
    let mut arena = KlibIrArena::default();
    let int = arena.push_type(T::Int.klib());
    let demo_package = ["demo".to_owned()];
    let box_classifier = metadata_class_signature(
        MetadataContainer {
            package: &demo_package,
            classes: &[],
            native_interop_library: false,
        },
        MetadataClass {
            name: "Box",
            type_params: &[],
            expect: false,
        },
    );
    let box_type = arena.push_type(KlibIrType::Simple {
        classifier: public_symbol(KlibIrSymbolKind::Class, box_classifier),
        nullability: KlibIrNullability::NotSpecified,
        arguments: Vec::new(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    assert_eq!(
        super::super::klib_types::semantic_type(&arena, &Default::default(), box_type)
            .expect("the fixture's class type is semantic"),
        Ty::obj_name(crate::types::type_name("demo/Box")),
        "the fixture's serialized receiver is demo.Box"
    );
    let nothing = arena.push_type(T::Nothing.klib());
    let mut slot = 9000;
    let mut parameter = |kind, name: &str, ty| {
        slot += 1;
        KlibIrParameter {
            base: base(local_symbol(kind, slot)),
            name: name.to_owned(),
            ty,
            vararg_element_type: None,
            default_value: None,
        }
    };
    let dispatch_receiver =
        dispatch.then(|| parameter(KlibIrSymbolKind::ReceiverParameter, "<this>", box_type));
    let context_parameters = (0..contexts)
        .map(|_| parameter(KlibIrSymbolKind::ValueParameter, "c", int))
        .collect::<Vec<_>>();
    let regular_parameters = (0..values)
        .map(|_| parameter(KlibIrSymbolKind::ValueParameter, "a", int))
        .collect::<Vec<_>>();
    let symbol = public_symbol(KlibIrSymbolKind::Function, signature);
    let returned = context_parameters
        .iter()
        .chain(&regular_parameters)
        .next()
        .expect("a parameter to return")
        .base
        .symbol
        .clone();
    let read = arena.push_expr(
        Some(int),
        KlibIrExprKind::GetValue {
            symbol: returned,
            origin: None,
        },
    );
    let ret = arena.push_expr(
        Some(nothing),
        KlibIrExprKind::Return {
            target: symbol.clone(),
            value: read,
        },
    );
    let function = arena.push_function(KlibIrFunction {
        base: base(symbol),
        name: name.to_owned(),
        constructor: false,
        type_parameters: Vec::new(),
        dispatch_receiver,
        context_parameters,
        extension_receiver: None,
        regular_parameters,
        return_type: int,
        overridden: Vec::new(),
        companion_extension_class: None,
        prepared_inline_file: None,
        body: Some(KlibIrBody::Block(vec![KlibIrStatement::Expression(ret)])),
    });
    KlibIrDeclarationTree {
        arena,
        declaration: KlibIrMember::Function(function),
    }
}

const CALLER: [Shape; 1] = [("a", &[T::Int], T::Int)];
const MEMBER_CALLER: [Shape; 1] = [("a", &[T::DemoBox, T::Int], T::Int)];

#[test]
fn a_call_no_reachable_library_serializes_declines_by_name() {
    let undeclared = demo_signature(None, "z", &[], &[T::Int]);
    let demo = demo(&CALLER, |_, body| {
        let a = body.param(0);
        let call = body.call_signature(undeclared.clone(), vec![a], None, T::Int);
        Some(vec![body.ret(call)])
    });
    assert_eq!(
        demo.declined("a", &[]),
        "the KLIB body of `demo.a` (it calls `demo.z`, which no reachable library serializes its declaration)"
    );
}

#[test]
fn a_member_call_declines_by_its_form() {
    let member = demo_signature(Some("Box"), "get", &[], &[T::Int]);
    let caller = |_: &str, body: &mut Body| {
        let receiver = body.param(0);
        let a = body.param(0);
        let call = body.call_signature(member.clone(), vec![receiver, a], None, T::Int);
        Some(vec![body.ret(call)])
    };
    let serialized = demo_with(
        &CALLER,
        vec![function_tree(member.clone(), "get", true, 0, 1)],
        caller,
    );
    assert_eq!(
        serialized.declined("a", &[]),
        "the KLIB body of `demo.a` (it uses a call of a member function)"
    );
    // A member the library does not serialize, as a fake override is not: the real declaration
    // is reached only through the serialized class declarations.
    let unserialized = demo(&CALLER, caller);
    assert_eq!(
        unserialized.declined("a", &[]),
        "the KLIB body of `demo.a` \
         (it uses a call of a member no library serializes, such as a fake override)"
    );
}

#[test]
fn a_later_support_unit_selects_the_matching_singleton_edge_variant() {
    let member = demo_signature(Some("Box"), "get", &[], &[T::Int]);
    let demo = demo_with(
        &MEMBER_CALLER,
        vec![function_tree(member.clone(), "get", true, 0, 1)],
        |_, body| {
            let receiver = body.param(0);
            let value = body.param(1);
            let call = body.call_signature(member.clone(), vec![receiver, value], None, T::Int);
            Some(vec![body.ret(call)])
        },
    );
    let mut selected_record = demo.facts["a"].clone();
    selected_record.name = "get".to_owned();
    selected_record.kind = ExternalCallableKind::Member;
    selected_record.params = vec![T::Int.semantic()];
    selected_record
        .generic_sig
        .as_mut()
        .expect("normalized signature")
        .params = vec![T::Int.semantic()];
    selected_record.parameter_identities =
        vec![ResolvedParameterIdentity::Source("a".into())].into_boxed_slice();
    selected_record.singleton_dispatch = Some(SingletonDispatch {
        classifier: crate::types::type_name("demo/Box"),
    });
    selected_record.declaration_signature = Some(KlibDeclarationSignature::Public(member.clone()));
    selected_record.dependency_callable = Some(KlibDependencyCallable {
        module: "demo".to_owned(),
        declaration: KlibDeclarationSignature::Public(member),
    });
    let mut inherited_record = selected_record.clone();
    inherited_record.singleton_dispatch = Some(SingletonDispatch {
        classifier: crate::types::type_name("demo/OtherBox"),
    });
    let selected = selected_record
        .klib_body_callable()
        .expect("the selected member is signed");
    let inherited = inherited_record
        .klib_body_callable()
        .expect("the inherited selection is signed to the same declaration");
    let box_ty = Ty::obj_name(crate::types::type_name("demo/Box"));
    assert_eq!(selected.singleton_dispatch(), Some(box_ty));
    assert_eq!(
        inherited.singleton_dispatch(),
        Some(Ty::obj_name(crate::types::type_name("demo/OtherBox")))
    );
    let external = crate::fir::ExternalCallableId::from_raw(31);
    let emitted = [(
        selected.identity().clone(),
        super::super::EmittedDependencyCallable::new(
            external,
            Some(box_ty),
            Some(crate::types::type_name("demo/Box")),
        ),
    )]
    .into_iter()
    .collect();
    let callees = KlibCalleeFacts::new([inherited, selected]);
    let mut unit = DependencyBodyUnit::default();
    unit.lower_function_with_intrinsics_and_externals(
        demo.callable("a"),
        &demo.bodies,
        &callees,
        crate::klib_libraries::KlibIntrinsicFacts::empty(),
        &emitted,
    )
    .expect("the serialized receiver selects the matching edge, independent of insertion order");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 1, "the member is not re-emitted");
    let calls = unit
        .ir()
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(expression, node)| match node {
            crate::ir::IrExpr::Call {
                callee: call_callee,
                dispatch_receiver,
                args,
            } if let crate::ir::Callee::External { target, .. } = &**call_callee => Some((
                crate::ir::ExprId::try_from(expression).expect("an expression id fits u32"),
                *target,
                *dispatch_receiver,
                args.len(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(call, target, Some(receiver), 1)] = calls.as_slice() else {
        panic!("expected one external singleton-member call, got {calls:?}");
    };
    assert_eq!(*target, external);
    assert_eq!(
        unit.ir()
            .logical_type(*receiver)
            .expect("the receiver has a checked type"),
        Ty::obj_name(crate::types::type_name("demo/Box"))
    );
    assert_eq!(
        unit.ir().external_singleton_dispatches.get(call),
        Some(&crate::types::type_name("demo/Box"))
    );
}

#[test]
fn a_root_group_publishes_the_singleton_variant_which_matches_its_header() {
    let member = demo_signature(Some("Box"), "get", &[], &[T::Int]);
    let demo = demo_with(
        &CALLER,
        vec![function_tree(member.clone(), "get", true, 0, 1)],
        |_, body| {
            let value = body.param(0);
            Some(vec![body.ret(value)])
        },
    );
    let mut selected_record = demo.facts["a"].clone();
    selected_record.name = "get".to_owned();
    selected_record.kind = ExternalCallableKind::Member;
    selected_record.singleton_dispatch = Some(SingletonDispatch {
        classifier: crate::types::type_name("demo/Box"),
    });
    selected_record.declaration_signature = Some(KlibDeclarationSignature::Public(member.clone()));
    selected_record.dependency_callable = Some(KlibDependencyCallable {
        module: "demo".to_owned(),
        declaration: KlibDeclarationSignature::Public(member),
    });
    let mut inherited_record = selected_record.clone();
    inherited_record.singleton_dispatch = Some(SingletonDispatch {
        classifier: crate::types::type_name("demo/OtherBox"),
    });
    let selected = selected_record
        .klib_body_callable()
        .expect("the selected member is signed");
    let inherited = inherited_record
        .klib_body_callable()
        .expect("the inherited selection is signed to the same declaration");
    let callees = KlibCalleeFacts::new([inherited, selected]);
    let externals = Default::default();
    let mut unit = DependencyBodyUnit::default();
    unit.lower_root_with_intrinsics_and_externals(
        inherited,
        &demo.bodies,
        &callees,
        crate::klib_libraries::KlibIntrinsicFacts::empty(),
        &externals,
    )
    .expect("the root header selects the matching edge, independent of insertion order");
    validated(unit.ir());
    assert_eq!(unit.ir().functions.len(), 1);
    assert_eq!(
        unit.authenticated_singleton_dispatch(selected.identity()),
        Some(Ty::obj_name(crate::types::type_name("demo/Box")))
    );
}

#[test]
fn a_header_alone_does_not_describe_a_context_parameter() {
    let contextual = demo_signature(None, "ctx", &[T::Int], &[]);
    let demo = demo_with(
        &CALLER,
        vec![function_tree(contextual.clone(), "ctx", false, 1, 0)],
        |_, body| {
            let a = body.param(0);
            let call = body.call_signature(contextual.clone(), vec![a], None, T::Int);
            Some(vec![body.ret(call)])
        },
    );
    assert_eq!(
        demo.declined("a", &[]),
        "the KLIB body of `demo.a` (it reaches the KLIB body of `demo.ctx` \
         (it declares a context parameter no selected declaration describes))"
    );
}

// --- The Kotlin/Native stdlib -------------------------------------------------------------------

/// The provider record of every non-generic public top-level stdlib function, with its package.
fn stdlib_records(
    root: &std::path::Path,
    libraries: &crate::klib_libraries::KlibLibraries,
) -> Vec<(String, crate::backend::BackendCallableFact)> {
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    let path = root.join("klib/common/stdlib");
    let archive = crate::klib::KlibArchive::open(&path).expect("the stdlib opens");
    let mut names = std::collections::BTreeSet::new();
    for fragment in archive.package_fragments() {
        let bytes = archive.read(&fragment.entry).expect("readable");
        let package =
            crate::metadata::semantic::parse_package_fragment_checked(&bytes).expect("decodes");
        for function in package.functions {
            if function.visibility != crate::types::Visibility::Private
                && function.formals.is_empty()
            {
                names.insert((fragment.package_fqname.replace('.', "/"), function.name));
            }
        }
    }
    let mut records = Vec::new();
    for (package, name) in names {
        let symbols = libraries.symbols(
            SymbolNamespace::Package(crate::types::type_name(&package)),
            &name,
        );
        for function in symbols.callables.functions() {
            let target = function
                .callable
                .external_identity
                .expect("a KLIB function carries its provider identity");
            let mut ir = crate::ir::IrFile::default();
            ir.add_expr(crate::ir::IrExpr::Call {
                callee: Box::new(crate::ir::Callee::External {
                    target,
                    default_provider: None,
                    params: function.callable.params.clone(),
                    ret: function.callable.ret,
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                    extension_receiver_parameter: None,
                }),
                dispatch_receiver: None,
                args: Vec::new(),
            });
            let fact = crate::backend::CheckedBackendCallables::freeze(&ir, libraries)
                .expect("the provider answers for its identity")
                .callable(target)
                .expect("frozen")
                .clone();
            records.push((package.clone(), fact));
        }
    }
    records
}

/// The stdlib under lowering with no selected callee: every callee a body reaches is declared from
/// its serialized header. The stdlib is closed over its own calls (no call names a declaration no
/// loaded library declares), and the bodies that call another function now lower past that call
/// and decline, if at all, inside the callee by the callee's own form.
#[test]
fn stdlib_calls_reach_header_declared_callees() {
    let Some(root) = super::distribution_root() else {
        return;
    };
    let libraries = super::stdlib(&root);
    let bodies = libraries.declaration_bodies();
    let mut reached = std::collections::BTreeMap::<String, usize>::new();
    let mut lowered_with_callees = Vec::new();
    let mut undeclared = Vec::new();
    for (package, record) in stdlib_records(&root, &libraries) {
        let mut unit = DependencyBodyUnit::default();
        let callable = record.klib_body_callable().expect("signed");
        match unit.lower_function(callable, bodies, &KlibCalleeFacts::default()) {
            Ok(_) if unit.ir().functions.len() > 1 => {
                lowered_with_callees.push(format!("{package}.{}", record.name));
            }
            Ok(_) => {}
            Err(decline) => {
                assert_unit_is_empty(&unit);
                if matches!(decline.reason(), KlibBodyDeclineReason::UndeclaredCallee(_)) {
                    undeclared.push(decline.to_string());
                }
                let mut innermost = &decline;
                while let KlibBodyDeclineReason::CalleeDeclined(callee) = innermost.reason() {
                    innermost = callee;
                }
                if !std::ptr::eq(innermost, &decline) {
                    let text = innermost.to_string();
                    let (_, reason) = text.split_once(" (").expect("a decline names its reason");
                    *reached.entry(reason.to_owned()).or_default() += 1;
                }
            }
        }
    }
    assert_eq!(undeclared, Vec::<String>::new());
    assert_eq!(lowered_with_callees, Vec::<String>::new());
    // Selected generic roots now pass their own declared headers and can reach unselected
    // generic callees. These exact deeper-decline counts are measured on every manifest release;
    // they are not expected-failure allowances for executable conformance tests. Elvis and safe-call
    // blocks, then generic calls and `when` subject blocks, now reach further declared callees;
    // the same complete inputs were measured on every release.
    let expected =
        if crate::kotlin_version::target() >= crate::kotlin_version::KotlinVersion::V2_4_20 {
            vec![
                ("it calls `kotlin.ArithmeticException.<init>`)", 2),
                ("it calls `kotlin.NumberFormatException.<init>`)", 4),
                ("it calls `kotlin.UInt.<init>`)", 4),
                ("it converts a value implicitly)", 1),
                ("it declares a parameter default)", 35),
                ("it declares type parameters)", 182),
                ("it uses a block of a compiler-introduced form)", 1),
                ("it uses a call of a member function)", 60),
                (
                    "it uses a call of a member no library serializes, such as a fake override)",
                    10,
                ),
                ("it uses a call through a property accessor signature)", 35),
                ("it uses a constant of another type than its value's)", 6),
                ("it uses a loop whose body is not a block)", 2),
                ("its library serializes none)", 74),
            ]
        } else {
            vec![
                ("it calls `kotlin.ArithmeticException.<init>`)", 2),
                ("it calls `kotlin.NumberFormatException.<init>`)", 4),
                ("it calls `kotlin.UInt.<init>`)", 4),
                ("it converts a value implicitly)", 1),
                ("it declares a parameter default)", 28),
                ("it declares type parameters)", 164),
                ("it uses a block of a compiler-introduced form)", 1),
                ("it uses a call of a member function)", 60),
                (
                    "it uses a call of a member no library serializes, such as a fake override)",
                    10,
                ),
                ("it uses a call through a property accessor signature)", 35),
                ("it uses a constant of another type than its value's)", 6),
                ("it uses a loop whose body is not a block)", 2),
                ("its library serializes none)", 74),
            ]
        };
    assert_eq!(
        reached.into_iter().collect::<Vec<_>>(),
        expected
            .into_iter()
            .map(|(reason, count)| (reason.to_owned(), count))
            .collect::<Vec<_>>()
    );
}

/// Bodies that declined at their first call before callees were declared from their headers, and
/// now decline inside the callee.
#[test]
fn stdlib_bodies_decline_inside_header_declared_callees() {
    let Some(root) = super::distribution_root() else {
        return;
    };
    let libraries = super::stdlib(&root);
    let bodies = libraries.declaration_bodies();
    let string = kotlin("String");
    let chars = kotlin("CharSequence");
    let int = kotlin("Int");
    let int_array = kotlin("IntArray");
    let cases = [
        (
            super::frozen_function(
                &libraries,
                "kotlin/text",
                "removePrefix",
                Some(string),
                &[chars],
            ),
            "the KLIB body of `kotlin.text.removePrefix` (it reaches the KLIB body of \
             `kotlin.text.startsWith` (it declares a parameter default))",
        ),
        (
            super::frozen_function(
                &libraries,
                "kotlin/collections",
                "contains",
                Some(int_array),
                &[int],
            ),
            "the KLIB body of `kotlin.collections.contains` (it reaches the KLIB body of \
             `kotlin.collections.indexOf` (it uses a call of a member no library serializes, such as a fake override))",
        ),
    ];
    let declines = cases
        .iter()
        .map(|(record, _)| {
            let mut unit = DependencyBodyUnit::default();
            match unit.lower_function(
                record.klib_body_callable().expect("signed"),
                bodies,
                &KlibCalleeFacts::default(),
            ) {
                Ok(function) => lowered_body(&unit, function),
                Err(decline) => {
                    assert_unit_is_empty(&unit);
                    decline.to_string()
                }
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        declines,
        cases
            .iter()
            .map(|(_, expected)| expected.to_owned())
            .collect::<Vec<_>>()
    );
}
