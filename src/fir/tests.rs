use super::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::diag::{DiagSink, Span};
use crate::features::LangFeatures;
use crate::source::SourceInput;
use crate::types::{Ty, Visibility};

fn anchor(source: SourceFileId, lo: u32, kind: DeclarationKind) -> DeclarationAnchor {
    DeclarationAnchor {
        source,
        range: Span::new(lo, lo + 1),
        owner: None,
        kind,
        sibling: 0,
    }
}

fn inferred_stub(id: DeclarationId, kind: InferredSignatureKind) -> DeclarationStub {
    let source = SourceFileId::from_raw(0);
    DeclarationStub {
        id,
        source,
        range: Span::new(0, 20),
        lookup_name: None,
        body: Some(BodyKind::Function),
        signature_inference: Some(kind),
        initialization_order: None,
        kind: DeclarationKind::Function,
        visibility: Visibility::Public,
        flags: DeclarationFlags::default(),
    }
}

fn body_stub(id: DeclarationId, inline: bool) -> DeclarationStub {
    let source = SourceFileId::from_raw(0);
    DeclarationStub {
        id,
        source,
        range: Span::new(0, 20),
        lookup_name: None,
        body: Some(BodyKind::Function),
        signature_inference: None,
        initialization_order: None,
        kind: DeclarationKind::Function,
        visibility: Visibility::Public,
        flags: DeclarationFlags::default().with(DeclarationFlags::INLINE, inline),
    }
}

fn body_work(stub: DeclarationStub) -> BodyWorkItem {
    BodyWorkItem {
        declaration: stub.id,
        owner: stub.body_owner(),
        kind: stub.body.expect("test declaration must own a body"),
    }
}

fn index_with_signatures(
    declarations: impl IntoIterator<Item = DeclarationId>,
) -> ResolvedModuleIndex {
    let declarations = declarations.into_iter().collect::<Vec<_>>();
    let states = declarations
        .iter()
        .copied()
        .map(|declaration| {
            (
                declaration,
                SignatureState::Resolved(ResolvedSignature::new([], Ty::Unit).unwrap()),
            )
        })
        .collect::<HashMap<_, _>>();
    finalize_signatures(declarations, states).unwrap()
}

#[test]
fn stable_declaration_ids_survive_reinterning_without_ast_ids_or_names() {
    let mut ids = DeclarationIds::default();
    let source = SourceFileId::from_raw(7);
    let first = ids.intern(anchor(source, 12, DeclarationKind::Function));
    let reparsed = ids.intern(anchor(source, 12, DeclarationKind::Function));
    let other = ids.intern(anchor(source, 12, DeclarationKind::Property));

    assert_eq!(first, reparsed);
    assert_ne!(first, other);
    assert_eq!(
        ids.anchor(first),
        Some(anchor(source, 12, DeclarationKind::Function))
    );
    assert_eq!(std::mem::size_of::<DeclarationId>(), 4);
}

#[test]
fn generated_capture_storage_inherits_its_exact_owner_source_order() {
    let source = SourceFileId::from_raw(0);
    let mut declarations = DeclarationIds::default();
    let owner = declarations.intern(anchor(source, 10, DeclarationKind::Classifier));
    let capture = declarations.intern(DeclarationAnchor {
        source,
        range: Span::new(10, 11),
        owner: Some(owner),
        kind: DeclarationKind::Property,
        sibling: u32::MAX,
    });
    let mut index = ResolvedModuleIndex::default();
    index.publish_declaration_header(
        owner,
        ResolvedDeclarationHeader {
            kind: DeclarationKind::Classifier,
            owner: None,
            name: None,
            visibility: Visibility::Public,
            flags: DeclarationFlags::default(),
            initialization_order: None,
        },
        Some("Local"),
    );
    index.publish_declaration_header(
        capture,
        ResolvedDeclarationHeader {
            kind: DeclarationKind::Property,
            owner: Some(owner),
            name: None,
            visibility: Visibility::Private,
            flags: DeclarationFlags::default().with(DeclarationFlags::COMPILER_GENERATED, true),
            initialization_order: None,
        },
        Some("captured"),
    );

    index.publish_source_inventory(&[owner], &declarations);

    assert_eq!(index.source_order(owner), Some(0));
    assert_eq!(index.source_order(capture), Some(0));
    assert_eq!(
        index.source_inventory(source),
        &[owner],
        "capture storage has no reparsed syntax declaration"
    );
}

#[test]
fn lookup_spelling_is_not_declaration_identity() {
    let mut diagnostics = DiagSink::new();
    let first =
        crate::frontend::parse_source_with_detected_features("fun alpha() = 1", &mut diagnostics);
    let renamed =
        crate::frontend::parse_source_with_detected_features("fun bravo() = 1", &mut diagnostics);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let mut ids = DeclarationIds::default();
    let mut names = LookupNames::default();
    let first = extract_file_stubs(&first, SourceFileId::from_raw(0), &mut ids, &mut names);
    let renamed = extract_file_stubs(&renamed, SourceFileId::from_raw(0), &mut ids, &mut names);

    assert_eq!(first[0].id, renamed[0].id);
    assert_ne!(first[0].lookup_name, renamed[0].lookup_name);
    assert_eq!(names.get(first[0].lookup_name.unwrap()), Some("alpha"));
    assert_eq!(names.get(renamed[0].lookup_name.unwrap()), Some("bravo"));
}

#[test]
fn pass_one_stubs_survive_ast_drop_and_reparse_with_the_same_ids() {
    let source_text = r#"
inline fun answer() = 42
fun ordinary(): Unit { val x = 1 }
class C {
    init { ordinary() }
    fun member() = answer()
    val computed get() = member()
}
"#;
    let mut diagnostics = crate::diag::DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(source_text, &mut diagnostics);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let source = SourceFileId::from_raw(0);
    let mut ids = DeclarationIds::default();
    let mut names = LookupNames::default();
    let first = extract_file_stubs(&file, source, &mut ids, &mut names);
    let first_ids = first.iter().map(|stub| stub.id).collect::<Vec<_>>();
    drop(file);

    let reparsed =
        crate::frontend::parse_source_with_detected_features(source_text, &mut diagnostics);
    let second = extract_file_stubs(&reparsed, source, &mut ids, &mut names);
    assert_eq!(
        second.iter().map(|stub| stub.id).collect::<Vec<_>>(),
        first_ids
    );
    assert_eq!(names.len(), 5, "reparsing must reuse lookup spellings");
    assert_eq!(
        first
            .iter()
            .filter(|stub| stub.flags.has(DeclarationFlags::INLINE))
            .count(),
        1
    );
    assert!(first.iter().all(|stub| stub.source == source));
    assert!(first.iter().any(|stub| {
        stub.kind == DeclarationKind::Initializer && stub.body == Some(BodyKind::Initializer)
    }));
    assert!(first.iter().any(|stub| {
        stub.kind == DeclarationKind::Accessor && stub.body == Some(BodyKind::Getter)
    }));
}

#[test]
fn streamed_header_inventory_returns_no_whole_module_ast() {
    let sources = [
        SourceInput::kotlin("fun first() = 1").with_file_stem("First"),
        SourceInput::kotlin("fun second(): Int { return 2 }").with_file_stem("Second"),
    ];
    let mut diagnostics = DiagSink::new();
    let mut visited = Vec::new();
    let module = stream_file_stub_inventory(
        &sources,
        &LangFeatures::new(),
        &mut diagnostics,
        |source, file, stubs| {
            visited.push((source, file.expr_arena.len(), stubs.len()));
            assert!(stubs.iter().all(|stub| stub.source == source));
        },
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert_eq!(visited.len(), 2);
    assert_eq!(visited[0].0, SourceFileId::from_raw(0));
    assert_eq!(visited[1].0, SourceFileId::from_raw(1));
    assert_eq!(module.sources.len(), 2);
    assert_eq!(module.stubs.len(), 2);
    assert_eq!(
        module.lookup_names.len(),
        3,
        "declaration spellings and explicit type lookup input are interned; bodies are not"
    );
    assert_eq!(
        module
            .stubs
            .iter()
            .map(|stub| {
                module
                    .lookup_names
                    .get(stub.lookup_name.expect("named top-level function"))
                    .unwrap()
            })
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert_eq!(
        module
            .sources
            .get(SourceFileId::from_raw(0))
            .unwrap()
            .path
            .as_ref(),
        "First.kt"
    );
}

#[test]
fn actualization_excludes_the_compact_expect_subtree_before_signatures() {
    let sources = [
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
                 expect class Api { fun hidden(): MissingExpectOnlyType }\n",
        )
        .with_file_stem("Common"),
        SourceInput::kotlin(
            "// LANGUAGE: +MultiPlatformProjects\n\
                 actual class Api { actual fun hidden(): String = \"OK\" }\n",
        )
        .with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let mut module = stream_file_stub_inventory(
        &sources,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);

    let matched = matched_expect_declarations(&module);
    assert_eq!(matched.len(), 1, "only the expect class root is selected");
    let expect = *matched.iter().next().unwrap();
    assert!(module.stubs.iter().any(|stub| {
        module
            .declarations
            .anchor(stub.id)
            .is_some_and(|anchor| anchor.owner == Some(expect))
    }));

    module.exclude_declaration_subtrees(&matched);
    assert!(module.stubs.iter().all(|stub| {
        let mut current = Some(stub.id);
        while let Some(declaration) = current {
            if declaration == expect {
                return false;
            }
            current = module
                .declarations
                .anchor(declaration)
                .and_then(|anchor| anchor.owner);
        }
        true
    }));
}

#[test]
fn streamed_headers_preserve_compact_package_and_import_scopes() {
    let inputs = [SourceInput::kotlin(
        "package sample.core\nimport alpha.beta.*\nimport gamma.Type as Alias\nannotation class Marker\n@Marker fun value() = 1",
    )
    .common()
    .with_file_stem("Scoped")];
    let mut diagnostics = DiagSink::new();
    let module = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let scope = module
        .scopes
        .file(SourceFileId::from_raw(0))
        .expect("compact file scope");
    assert!(scope.is_common);
    let render_path = |range| {
        module
            .scopes
            .path(range)
            .iter()
            .map(|name| module.lookup_names.get(*name).unwrap())
            .collect::<Vec<_>>()
            .join(".")
    };
    assert_eq!(render_path(scope.package), "sample.core");
    let imports = module.scopes.imports(scope.imports);
    assert_eq!(imports.len(), 2);
    assert_eq!(render_path(imports[0].path), "alpha.beta");
    assert!(imports[0].wildcard);
    assert_eq!(render_path(imports[1].path), "gamma.Type");
    assert_eq!(
        imports[1]
            .alias
            .and_then(|alias| module.lookup_names.get(alias)),
        Some("Alias")
    );
    let detached = module
        .detached_type_roots(SourceFileId::from_raw(0))
        .filter_map(|ty| module.syntax.transient_type_ref(ty, &module.lookup_names))
        .map(|ty| ty.name)
        .collect::<Vec<_>>();
    assert!(
        detached.iter().any(|name| name == "Marker"),
        "declaration annotation types must survive only in packed Pass-1 syntax"
    );
}

#[test]
fn compact_header_types_own_same_file_alias_source_spellings_by_type_identity() {
    let inputs = [SourceInput::kotlin(
        "package app\nclass Payload\ntypealias Cargo = Payload\nfun make(value: Cargo): Cargo = value",
    )];
    let mut diagnostics = DiagSink::new();
    let module = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let function = module
        .stubs
        .iter()
        .find(|stub| {
            stub.kind == DeclarationKind::Function
                && stub
                    .lookup_name
                    .and_then(|name| module.lookup_names.get(name))
                    == Some("make")
        })
        .expect("make header");
    let HeaderDeclarationKind::Callable {
        parameters,
        result: HeaderResultType::Explicit(result),
        ..
    } = module.syntax.declaration(function.id).unwrap().kind
    else {
        panic!("explicit callable header")
    };
    let parameter = module.syntax.parameters(parameters)[0].ty;
    for ty in [parameter, result] {
        let expanded = module
            .syntax
            .transient_type_ref(ty, &module.lookup_names)
            .expect("expanded compact type");
        assert_eq!(expanded.name, "Payload");
        let spellings = module
            .syntax
            .transient_source_spellings(ty, &module.lookup_names)
            .expect("source spelling projection");
        assert_eq!(
            spellings.get(&expanded.span).map(|ty| ty.name.as_str()),
            Some("Cargo")
        );
    }
}

#[test]
fn streamed_headers_pack_explicit_type_syntax_without_ast_ids() {
    let inputs = [SourceInput::kotlin(
        r#"
typealias Projection<T> = Map<in T, out List<T?>>
fun <reified T : Any> T.use(
    callback: suspend context(String) T.(Int) -> List<*>,
    fallback: Int = 1
): T & Any = this
class Box<out V : Number>(val value: V, plain: Int = 1) : Base<V>()
"#,
    )];
    let mut diagnostics = DiagSink::new();
    let module = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);

    let use_stub = module
        .stubs
        .iter()
        .find(|stub| {
            stub.lookup_name
                .and_then(|name| module.lookup_names.get(name))
                == Some("use")
        })
        .expect("extension function stub");
    let HeaderDeclarationKind::Callable {
        receiver,
        parameters,
        result,
        type_parameters,
        bounds,
        context_count,
        ..
    } = module.syntax.declaration(use_stub.id).unwrap().kind
    else {
        panic!("callable header")
    };
    assert_eq!(context_count, 0);
    assert!(receiver.is_some());
    assert!(matches!(result, HeaderResultType::Explicit(_)));
    assert_eq!(module.syntax.type_parameters(type_parameters).len(), 1);
    assert!(module.syntax.type_parameters(type_parameters)[0]
        .flags
        .is_reified());
    assert_eq!(module.syntax.bounds(bounds).len(), 1);

    let parameters = module.syntax.parameters(parameters);
    assert_eq!(parameters.len(), 2);
    assert!(!parameters[0].flags.has_default());
    assert!(parameters[1].flags.has_default());
    let callback = module.syntax.ty(parameters[0].ty).unwrap();
    assert!(callback.flags.suspend_function());
    assert!(callback.flags.function_receiver());
    let transient = module
        .syntax
        .transient_type_ref(parameters[0].ty, &module.lookup_names)
        .unwrap();
    assert_eq!(transient.name, "<fun>");
    assert!(transient.fun_suspend());
    assert!(transient.fun_has_receiver());
    assert_eq!(transient.fun_context_count, 1);
    let HeaderTypeKind::Function {
        parameters,
        result: Some(result),
        context_count,
    } = callback.kind
    else {
        panic!("function type header")
    };
    assert_eq!(context_count, 1);
    assert_eq!(module.syntax.type_operands(parameters).len(), 3);
    let HeaderTypeKind::Classifier { detail, .. } = module.syntax.ty(result).unwrap().kind else {
        panic!("function result classifier")
    };
    let arguments = module.syntax.classifier_type(detail).unwrap().arguments;
    let star = module.syntax.type_operands(arguments)[0];
    assert!(module.syntax.ty(star).unwrap().flags.star_projection());

    let alias_stub = module
        .stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::TypeAlias)
        .unwrap();
    let HeaderDeclarationKind::TypeAlias { target, .. } =
        module.syntax.declaration(alias_stub.id).unwrap().kind
    else {
        panic!("type alias header")
    };
    let HeaderTypeKind::Classifier { detail, .. } = module.syntax.ty(target).unwrap().kind else {
        panic!("alias classifier")
    };
    let HeaderClassifierType { path, arguments } = module.syntax.classifier_type(detail).unwrap();
    assert_eq!(
        module
            .syntax
            .type_path(path)
            .iter()
            .map(|name| module.lookup_names.get(*name).unwrap())
            .collect::<Vec<_>>(),
        ["Map"]
    );
    let projections = module.syntax.type_operands(arguments);
    assert!(module
        .syntax
        .ty(projections[0])
        .unwrap()
        .flags
        .in_projection());
    assert!(module
        .syntax
        .ty(projections[1])
        .unwrap()
        .flags
        .out_projection());
    let box_stub = module
        .stubs
        .iter()
        .find(|stub| {
            stub.lookup_name
                .and_then(|name| module.lookup_names.get(name))
                == Some("Box")
        })
        .expect("classifier stub");
    let HeaderDeclarationKind::Classifier {
        supertypes, base, ..
    } = module.syntax.declaration(box_stub.id).unwrap().kind
    else {
        panic!("classifier header")
    };
    assert!(module.syntax.type_operands(supertypes).is_empty());
    assert!(
        base.is_some(),
        "superclass must not be flattened into interfaces"
    );
    assert!(std::mem::size_of::<HeaderType>() <= 32);
}

#[test]
fn streamed_headers_pack_interface_delegation_as_ordinals() {
    let inputs = [SourceInput::kotlin(
        "interface Contract\nclass Wrapper(delegate: Contract) : Contract by delegate\n",
    )];
    let mut diagnostics = DiagSink::new();
    let module = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let wrapper = module
        .stubs
        .iter()
        .find(|stub| {
            stub.lookup_name
                .and_then(|name| module.lookup_names.get(name))
                == Some("Wrapper")
        })
        .expect("wrapper classifier");
    let HeaderDeclarationKind::Classifier { delegations, .. } =
        module.syntax.declaration(wrapper.id).unwrap().kind
    else {
        panic!("classifier header")
    };
    let [delegation] = module.syntax.interface_delegations(delegations) else {
        panic!("one compact interface delegation")
    };
    assert_eq!(delegation.supertype, 0);
    assert_eq!(
        delegation.source,
        HeaderInterfaceDelegateSource::ConstructorParameter(0)
    );
}

#[test]
fn streamed_headers_keep_typealiased_delegation_bound_to_its_supertype() {
    let inputs = [SourceInput::kotlin(
        "interface Contract<T>\n\
         typealias Alias<T> = Contract<T>\n\
         class Wrapper<T>(delegate: Alias<T>) : Alias<T> by delegate\n",
    )];
    let mut diagnostics = DiagSink::new();
    let module = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let wrapper = module
        .stubs
        .iter()
        .find(|stub| {
            stub.lookup_name
                .and_then(|name| module.lookup_names.get(name))
                == Some("Wrapper")
        })
        .expect("wrapper classifier");
    let HeaderDeclarationKind::Classifier {
        supertypes,
        delegations,
        ..
    } = module.syntax.declaration(wrapper.id).unwrap().kind
    else {
        panic!("classifier header")
    };
    assert_eq!(module.syntax.type_operands(supertypes).len(), 1);
    let [delegation] = module.syntax.interface_delegations(delegations) else {
        panic!("one compact interface delegation")
    };
    assert_eq!(delegation.supertype, 0);
    assert_eq!(
        delegation.source,
        HeaderInterfaceDelegateSource::ConstructorParameter(0)
    );
}

#[test]
fn default_expression_growth_does_not_enter_header_syntax_or_signature_constraints() {
    fn inventory(default: &str) -> StreamedHeaderModule {
        let source = format!("fun value(input: Int = {default}): Int = input");
        let inputs = [SourceInput::kotlin(&source)];
        let mut diagnostics = DiagSink::new();
        let module = stream_file_stub_inventory(
            &inputs,
            &LangFeatures::new(),
            &mut diagnostics,
            |_, _, _| {},
        );
        assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
        module
    }

    let small = inventory("1");
    let large_expression = std::iter::repeat_n("1 + ", 100).collect::<String>() + "1";
    let large = inventory(&large_expression);
    assert_eq!(small.syntax.type_count(), large.syntax.type_count());
    assert_eq!(
        small.syntax.storage_payload_bytes(),
        large.syntax.storage_payload_bytes(),
        "default expression AST must remain body work, not compact signature syntax"
    );
    assert!(small
        .stubs
        .iter()
        .all(|stub| stub.signature_inference.is_none()));
    let declaration = small.stubs[0].id;
    let HeaderDeclarationKind::Callable { parameters, .. } =
        small.syntax.declaration(declaration).unwrap().kind
    else {
        panic!("callable header")
    };
    assert!(small.syntax.parameters(parameters)[0].flags.has_default());
}

#[test]
fn annotation_policy_inventory_flattens_named_target_arrays_without_body_ids() {
    let text =
        "@Target(allowedTargets = [AnnotationTarget.FIELD, AnnotationTarget.VALUE_PARAMETER])\n\
                annotation class Mark\n";
    let inputs = [SourceInput::kotlin(text)];
    let mut diagnostics = DiagSink::new();
    let module = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let declaration = module
        .stubs
        .iter()
        .find(|stub| {
            stub.lookup_name
                .and_then(|name| module.lookup_names.get(name))
                == Some("Mark")
        })
        .expect("annotation class declaration")
        .id;
    let [application] = module.annotation_policy_applications(declaration) else {
        panic!("one compact annotation policy application")
    };
    let arguments = module
        .annotation_policy_arguments(application.arguments)
        .iter()
        .map(|argument| module.lookup_names.get(*argument).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(arguments, ["FIELD", "VALUE_PARAMETER"]);
}

#[test]
fn streamed_extractor_builds_lazy_call_and_member_constraints_from_the_transient_ast() {
    let text = "fun a() = b().length\nfun b() = \"hello\"";
    let sources = [SourceInput::kotlin(text).with_file_stem("Lazy")];
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(text, &mut diagnostics);
    let headers = inventory_parsed_source_headers(&sources, std::slice::from_ref(&file));
    let mut origins = OriginStore::default();
    let mut extractor = SignatureConstraintExtractor::default();
    extractor.extract_file(&file, SourceFileId::from_raw(0), &headers.stubs, |span| {
        origins.source(SourceFileId::from_raw(0), span)
    });
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert_eq!(extractor.failures(), []);

    let graph = extractor.finish().unwrap();
    let declaration = |name: &str| {
        headers
            .stubs
            .iter()
            .find(|stub| {
                stub.lookup_name
                    .and_then(|name| headers.lookup_names.get(name))
                    == Some(name)
            })
            .unwrap()
            .id
    };
    let a = graph.constraint(declaration("a")).unwrap();
    let b = graph.constraint(declaration("b")).unwrap();
    let SigExpr::Member {
        receiver, lookup, ..
    } = graph.expr(a.result).unwrap()
    else {
        panic!("a must remain a member lookup over b's lazy call")
    };
    let Some(SigExpr::Call { target, .. }) = graph.expr(receiver) else {
        panic!("member receiver must remain b's lazy call")
    };
    let call = graph.callable_selection(target).expect("call selection");
    let callee_start = text.find("b()").expect("callee spelling") as u32;
    assert_eq!(
        origins.get(call.origin),
        Some(Origin::Source {
            file: SourceFileId::from_raw(0),
            span: Span::new(callee_start, callee_start + 1),
        }),
        "a deferred call diagnostic must point at the callee, not its argument list"
    );
    let lookup = graph.member_selection(lookup).expect("member selection");
    let selector_start = text.find("length").expect("selector spelling") as u32;
    assert_eq!(
        origins.get(lookup.origin),
        Some(Origin::Source {
            file: SourceFileId::from_raw(0),
            span: Span::new(selector_start, selector_start + "length".len() as u32),
        }),
        "a deferred member diagnostic must point at the selector, not the whole receiver chain"
    );
    assert!(matches!(graph.expr(b.result), Some(SigExpr::Known(ty)) if ty.get() == Ty::String));
    assert_eq!(graph.node_count(), 3);
    // Each inferred body retains a constraint origin and its root-node origin; the member
    // expression additionally retains the receiver call and exact selector origins for deferred
    // selection and diagnostics.
    assert_eq!(origins.len(), 8);
}

#[test]
fn streamed_extractor_resolves_non_generic_local_typealiases_without_retaining_syntax() {
    let text = r#"
fun localAlias(flag: Boolean) =
    if (flag) {
        typealias Text = String
        val text: Text = "alias"
        text
    } else {
        "fallback"
    }
"#;
    let sources = [SourceInput::kotlin(text).with_file_stem("LocalAlias")];
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(text, &mut diagnostics);
    let headers = inventory_parsed_source_headers(&sources, std::slice::from_ref(&file));
    let mut origins = OriginStore::default();
    let mut extractor = SignatureConstraintExtractor::default();
    extractor.extract_file(&file, SourceFileId::from_raw(0), &headers.stubs, |span| {
        origins.source(SourceFileId::from_raw(0), span)
    });

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    assert_eq!(extractor.failures(), []);
    let graph = extractor.finish().unwrap();
    assert_eq!(graph.constraints().len(), 1);
    assert!(graph.node_count() > 0);
    drop(file);
    assert_eq!(graph.constraints()[0].declaration, headers.stubs[0].id);
}

struct TestHeaderSemantics {
    operations: Vec<(DeclarationKind, DeclarationId)>,
    classifier_failure: Option<DiagnosticId>,
}

impl ExplicitHeaderSemantics for TestHeaderSemantics {
    fn resolve_callable(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<ResolvedSignature, DiagnosticId> {
        assert!(matches!(
            declaration.kind,
            HeaderDeclarationKind::Callable { .. }
        ));
        assert!(context.scopes.file(source).is_some());
        self.operations
            .push((DeclarationKind::Function, declaration.declaration));
        Ok(ResolvedSignature::new([Ty::Int], Ty::String).unwrap())
    }

    fn resolve_property(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<ResolvedSignature, DiagnosticId> {
        assert!(matches!(
            declaration.kind,
            HeaderDeclarationKind::Property { .. }
        ));
        assert!(context.scopes.file(source).is_some());
        self.operations
            .push((DeclarationKind::Property, declaration.declaration));
        Ok(ResolvedSignature::new([], Ty::String).unwrap())
    }

    fn resolve_constructor(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<ResolvedSignature, DiagnosticId> {
        assert!(matches!(
            declaration.kind,
            HeaderDeclarationKind::Constructor { .. }
        ));
        assert!(context.scopes.file(source).is_some());
        self.operations
            .push((DeclarationKind::Constructor, declaration.declaration));
        Ok(ResolvedSignature::new([], Ty::Unit).unwrap())
    }

    fn validate_classifier(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<(), DiagnosticId> {
        assert!(matches!(
            declaration.kind,
            HeaderDeclarationKind::Classifier { .. }
        ));
        assert!(context.scopes.file(source).is_some());
        self.operations
            .push((DeclarationKind::Classifier, declaration.declaration));
        self.classifier_failure.map_or(Ok(()), Err)
    }

    fn validate_type_alias(
        &mut self,
        declaration: HeaderDeclaration,
        source: SourceFileId,
        context: &HeaderResolutionContext<'_>,
    ) -> Result<(), DiagnosticId> {
        assert!(matches!(
            declaration.kind,
            HeaderDeclarationKind::TypeAlias { .. }
        ));
        assert!(context.scopes.file(source).is_some());
        self.operations
            .push((DeclarationKind::TypeAlias, declaration.declaration));
        Ok(())
    }
}

#[test]
fn explicit_headers_use_one_semantic_adapter_while_inferred_headers_stay_lazy() {
    let inputs = [SourceInput::kotlin(
        r#"
fun inferred() = 1
fun explicit(input: Int = 1): String = input.toString()
val inferredProperty = 1
val explicitProperty: String = "value"
class Box
typealias Alias = String
"#,
    )];
    let mut diagnostics = DiagSink::new();
    let module = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let required = module
        .stubs
        .iter()
        .filter(|stub| {
            matches!(
                stub.kind,
                DeclarationKind::Function
                    | DeclarationKind::Property
                    | DeclarationKind::Constructor
            )
        })
        .map(|stub| stub.id)
        .collect::<Vec<_>>();
    let mut solver = SignatureSolver::new(SignatureGraph::default(), required);
    let mut semantics = TestHeaderSemantics {
        operations: Vec::new(),
        classifier_failure: None,
    };
    solver.resolve_explicit_headers(
        &module.stubs,
        HeaderResolutionContext {
            syntax: &module.syntax,
            names: &module.lookup_names,
            scopes: &module.scopes,
            declarations: &module.declarations,
        },
        &mut semantics,
    );

    let inferred = module
        .stubs
        .iter()
        .filter(|stub| stub.signature_inference.is_some())
        .map(|stub| stub.id)
        .collect::<Vec<_>>();
    assert_eq!(inferred.len(), 2);
    assert!(inferred
        .iter()
        .all(|declaration| solver.state(*declaration) == Some(&SignatureState::Uncomputed)));
    let explicit = module
        .stubs
        .iter()
        .filter(|stub| {
            stub.signature_inference.is_none()
                && matches!(
                    stub.kind,
                    DeclarationKind::Function
                        | DeclarationKind::Property
                        | DeclarationKind::Constructor
                )
        })
        .map(|stub| stub.id)
        .collect::<Vec<_>>();
    assert!(explicit.iter().all(|declaration| matches!(
        solver.state(*declaration),
        Some(SignatureState::Resolved(_))
    )));
    assert_eq!(
        semantics
            .operations
            .iter()
            .map(|(kind, _)| *kind)
            .collect::<Vec<_>>(),
        vec![
            DeclarationKind::Function,
            DeclarationKind::Property,
            DeclarationKind::Classifier,
            DeclarationKind::Constructor,
            DeclarationKind::TypeAlias,
        ]
    );
}

#[test]
fn explicit_classifier_failure_blocks_signature_finalization() {
    let inputs = [SourceInput::kotlin("class Broken")];
    let mut diagnostics = DiagSink::new();
    let module = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let classifier = module
        .stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::Classifier)
        .unwrap()
        .id;
    let mut solver = SignatureSolver::new(SignatureGraph::default(), []);
    let mut headers = TestHeaderSemantics {
        operations: Vec::new(),
        classifier_failure: Some(DiagnosticId::from_raw(77)),
    };
    solver.resolve_explicit_headers(
        &module.stubs,
        HeaderResolutionContext {
            syntax: &module.syntax,
            names: &module.lookup_names,
            scopes: &module.scopes,
            declarations: &module.declarations,
        },
        &mut headers,
    );
    let expression_semantics = TestSignatureSemantics::new();
    assert_eq!(
        solver.finalize(&ResolverBackedSignatureEvaluator::new(
            &expression_semantics
        )),
        Err(SignatureFinalizationError {
            declarations: vec![(classifier, Some(DiagnosticId::from_raw(77)))],
        })
    );
}

#[test]
fn header_finalization_drops_lookup_state_and_publishes_stable_anchors() {
    let inputs =
        [
            SourceInput::kotlin(
                "package sample\nimport kotlin.Int as Number\nfun value(): Int = 1",
            )
            .with_file_stem("Finalized"),
        ];
    let mut diagnostics = DiagSink::new();
    let headers = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let declaration = headers.stubs[0].id;
    let anchor = headers.declarations.anchor(declaration).unwrap();
    let signatures = finalize_signatures(
        [declaration],
        HashMap::from([(
            declaration,
            SignatureState::Resolved(ResolvedSignature::new([], Ty::Int).unwrap()),
        )]),
    )
    .unwrap();

    let (index, sources, bodies) = headers.finish(signatures);
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies.into_iter().collect::<Vec<_>>(),
        vec![BodyWorkItem {
            declaration,
            owner: BodyOwnerId::from_raw(declaration.raw()),
            kind: BodyKind::Function,
        }]
    );
    let frontend = FrontendModule::new(
        index,
        InlineBodyStore::default(),
        DefaultArgumentStore::default(),
        sources,
    );
    assert_eq!(frontend.index().declaration_count(), 1);
    assert_eq!(
        frontend.index().declaration_anchor(declaration),
        Some(StableDeclarationAnchor::from(anchor))
    );
    assert_eq!(
        frontend
            .index()
            .signature(declaration)
            .unwrap()
            .result
            .get(),
        Ty::Int
    );
}

#[test]
fn streamed_headers_copy_annotation_strings_by_declaration_ordinal() {
    let inputs = [SourceInput::kotlin(
        "import kotlin.jvm.JvmName\n@JvmName(\"physicalName\")\nfun sourceName() = Unit",
    )];
    let mut diagnostics = DiagSink::new();
    let headers = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let function = headers
        .stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::Function)
        .unwrap();

    assert_eq!(
        headers
            .annotation_string_arguments(function.id, 0)
            .iter()
            .map(|argument| argument.as_ref())
            .collect::<Vec<_>>(),
        ["physicalName"]
    );
}

#[test]
fn streamed_callable_headers_own_declaration_and_parameter_annotation_references() {
    let inputs = [SourceInput::kotlin(
        "annotation class Decl\n\
         annotation class Value\n\
         annotation class TypeUse\n\
         @Decl fun selected(@Value input: @TypeUse Int): Int = input",
    )];
    let mut diagnostics = DiagSink::new();
    let headers = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let function = headers
        .stubs
        .iter()
        .find(|stub| {
            stub.kind == DeclarationKind::Function
                && stub
                    .lookup_name
                    .and_then(|name| headers.lookup_names.get(name))
                    == Some("selected")
        })
        .expect("selected header");
    let declaration = headers.syntax.declaration(function.id).unwrap();
    let HeaderDeclarationKind::Callable { parameters, .. } = declaration.kind else {
        panic!("callable header")
    };
    let spelling = |ty| {
        headers
            .syntax
            .transient_type_ref(ty, &headers.lookup_names)
            .unwrap()
            .name
    };
    assert_eq!(
        headers
            .syntax
            .type_operands(declaration.annotations)
            .iter()
            .copied()
            .map(spelling)
            .collect::<Vec<_>>(),
        ["Decl"]
    );
    let parameter = headers.syntax.parameters(parameters)[0];
    assert_eq!(
        headers
            .syntax
            .type_operands(parameter.annotations)
            .iter()
            .copied()
            .map(spelling)
            .collect::<Vec<_>>(),
        ["Value"]
    );
    assert_eq!(
        headers
            .syntax
            .type_operands(parameter.type_annotations)
            .iter()
            .copied()
            .map(spelling)
            .collect::<Vec<_>>(),
        ["TypeUse"]
    );
}

#[test]
fn pass_two_schedule_prepares_all_inline_units_before_ordinary_units() {
    let inputs = [SourceInput::kotlin(
        "inline fun fast(): Int = 1\nfun slow(): Int = 2",
    )];
    let mut diagnostics = DiagSink::new();
    let headers = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let declarations = headers.stubs.iter().map(|stub| stub.id).collect::<Vec<_>>();
    let states = declarations
        .iter()
        .copied()
        .map(|declaration| {
            (
                declaration,
                SignatureState::Resolved(ResolvedSignature::new([], Ty::Int).unwrap()),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut index = finalize_signatures(declarations.iter().copied(), states).unwrap();
    for (ordinal, stub) in headers.stubs.iter().enumerate() {
        index.publish_function(
            CallableId::from_raw(ordinal as u32),
            stub.id,
            headers.lookup_names.get(stub.lookup_name.unwrap()).unwrap(),
            stub.flags.has(DeclarationFlags::INLINE),
        );
    }

    let (index, _, bodies) = headers.finish(index);
    let schedule = bodies.partition_by_inline(&index);
    assert_eq!(schedule.inline.len(), 1);
    assert_eq!(schedule.ordinary.len(), 1);
    assert_eq!(
        schedule.inline.into_iter().next().unwrap().declaration,
        declarations[0]
    );
    assert_eq!(
        schedule.ordinary.into_iter().next().unwrap().declaration,
        declarations[1]
    );
}

#[test]
fn streamed_header_memory_is_independent_of_ordinary_body_ast_size() {
    fn inventory(source: &str) -> StreamedHeaderModule {
        let inputs = [SourceInput::kotlin(source).with_file_stem("Work")];
        let mut diagnostics = DiagSink::new();
        let module = stream_file_stub_inventory(
            &inputs,
            &LangFeatures::new(),
            &mut diagnostics,
            |_, _, _| {},
        );
        assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
        module
    }

    let small = inventory("fun work(): Int { val x0 = 0; return x0 }");
    let statements = (0..100)
        .map(|index| format!("val x{index} = {index}\n"))
        .collect::<String>();
    let large = inventory(&format!("fun work(): Int {{ {statements} return x99 }}"));
    assert_eq!(small.stubs.len(), large.stubs.len());
    assert_eq!(
        small.storage_payload_bytes(),
        large.storage_payload_bytes(),
        "transient ordinary-body AST growth must not enter the header module"
    );
}

#[test]
fn explicitly_typed_ordinary_body_size_does_not_change_stub_storage() {
    let short = "fun work(): Int { return 1 }";
    let statements = std::iter::repeat_n("val x = 1\n", 100).collect::<String>();
    let long = format!("fun work(): Int {{ {statements} return 1 }}");
    let extract = |text: &str| {
        let mut diagnostics = crate::diag::DiagSink::new();
        let file = crate::frontend::parse_source_with_detected_features(text, &mut diagnostics);
        assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
        extract_file_stubs(
            &file,
            SourceFileId::from_raw(0),
            &mut DeclarationIds::default(),
            &mut LookupNames::default(),
        )
    };

    let short_stubs = extract(short);
    let long_stubs = extract(&long);
    assert_eq!(short_stubs.len(), 1);
    assert_eq!(long_stubs.len(), 1);
    assert_eq!(
        std::mem::size_of_val(short_stubs.as_slice()),
        std::mem::size_of_val(long_stubs.as_slice())
    );
}

#[test]
fn pass_one_marks_only_non_local_expression_inferred_signatures() {
    let source_text = r#"// LANGUAGE: +ExplicitBackingFields
fun inferred() = 1
fun typed(): Int = 1
fun implicitUnit() { val local = 1 }
fun defaultOnly(value: Int = 1): Int = value
interface Defaults { fun bodylessDefault(value: Int = 1): Int }
val inferredProperty = 1
val typedProperty: Int = 1
val narrowedStorage: Any
    field = "value"
val computed get() = 1
val delegated by lazy { 1 }
fun String.extension() = length
val String.extensionProperty get() = length
"#;
    let mut diagnostics = crate::diag::DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(source_text, &mut diagnostics);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let mut ids = DeclarationIds::default();
    let stubs = extract_file_stubs(
        &file,
        SourceFileId::from_raw(0),
        &mut ids,
        &mut LookupNames::default(),
    );
    let inferred = stubs
        .iter()
        .filter_map(|stub| stub.signature_inference.map(|kind| (kind, stub)))
        .collect::<Vec<_>>();

    assert_eq!(
        inferred.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
        vec![
            InferredSignatureKind::ExpressionFunction,
            InferredSignatureKind::PropertyInitializer,
            InferredSignatureKind::BackingFieldInitializer,
            InferredSignatureKind::ExpressionGetter,
            InferredSignatureKind::DelegatedProperty,
            InferredSignatureKind::ExtensionExpression,
            InferredSignatureKind::ExtensionExpression,
        ]
    );
    assert!(inferred.iter().any(|(kind, stub)| {
        *kind == InferredSignatureKind::ExpressionGetter && stub.body.is_none()
    }));
    assert!(inferred.iter().any(|(kind, stub)| {
        *kind == InferredSignatureKind::DelegatedProperty && stub.body == Some(BodyKind::Delegate)
    }));
    assert!(inferred.iter().any(|(kind, stub)| {
        *kind == InferredSignatureKind::BackingFieldInitializer
            && stub.body == Some(BodyKind::Initializer)
    }));
    let defaults = stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::Classifier)
        .expect("Defaults classifier stub");
    assert!(stubs.iter().any(|stub| {
        stub.kind == DeclarationKind::Function
            && ids
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner == Some(defaults.id))
            && stub.signature_inference.is_none()
            && stub.body == Some(BodyKind::Function)
    }));
    assert!(
        stubs
            .iter()
            .filter(|stub| stub.signature_inference.is_none())
            .count()
            >= 4
    );
}

#[test]
fn declaration_stubs_preserve_fixed_size_semantic_header_flags() {
    let source_text = r#"
private inline suspend operator fun String.invoke(): String = this
open class Box(open var value: Int) {
    private lateinit var label: String
}
"#;
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(source_text, &mut diagnostics);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let mut names = LookupNames::default();
    let stubs = extract_file_stubs(
        &file,
        SourceFileId::from_raw(0),
        &mut DeclarationIds::default(),
        &mut names,
    );
    let named = |name: &str| {
        stubs
            .iter()
            .find(|stub| {
                stub.lookup_name.and_then(|id| names.get(id)) == Some(name)
                    && stub.kind != DeclarationKind::Classifier
            })
            .copied()
            .unwrap()
    };
    let invoke = named("invoke");
    assert_eq!(invoke.visibility, Visibility::Private);
    assert!(invoke.flags.has(DeclarationFlags::INLINE));
    assert!(invoke.flags.has(DeclarationFlags::SUSPEND));
    assert!(invoke.flags.has(DeclarationFlags::OPERATOR));

    let classifier = stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::Classifier)
        .copied()
        .unwrap();
    assert_eq!(classifier.visibility, Visibility::Public);
    assert!(classifier.flags.has(DeclarationFlags::OPEN));
    assert!(!classifier.flags.has(DeclarationFlags::FINAL));

    let value = named("value");
    assert!(value.flags.has(DeclarationFlags::PROPERTY_PARAMETER));
    assert!(value.flags.has(DeclarationFlags::MUTABLE));
    assert!(value.flags.has(DeclarationFlags::OPEN));
    let label = named("label");
    assert_eq!(label.visibility, Visibility::Private);
    assert!(label.flags.has(DeclarationFlags::MUTABLE));
    assert!(label.flags.has(DeclarationFlags::LATEINIT));
    assert_eq!(std::mem::size_of::<DeclarationFlags>(), 8);
}

#[test]
fn stub_inventory_includes_non_decl_arena_declaration_forms() {
    let source_text = r#"
typealias Top = String
class Holder(val stored: Int, plain: Int) {
    typealias Nested = String
    companion object {
        fun make() = Holder(1, 2)
        val label = "holder"
    }
}
enum class Choice(val code: Int) {
    ONE(1) {
        override fun value() = code
        val label = "one"
        init { value() }
    };
    abstract fun value(): Int
}
"#;
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(source_text, &mut diagnostics);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let mut ids = DeclarationIds::default();
    let stubs = extract_file_stubs(
        &file,
        SourceFileId::from_raw(0),
        &mut ids,
        &mut LookupNames::default(),
    );

    assert_eq!(
        stubs
            .iter()
            .filter(|stub| stub.kind == DeclarationKind::TypeAlias)
            .count(),
        2
    );
    let companion = stubs
        .iter()
        .find(|stub| {
            stub.kind == DeclarationKind::Classifier
                && ids
                    .anchor(stub.id)
                    .is_some_and(|anchor| anchor.owner.is_some())
        })
        .expect("companion classifier stub");
    assert!(stubs.iter().any(|stub| {
        ids.anchor(stub.id)
            .is_some_and(|anchor| anchor.owner == Some(companion.id))
            && stub.signature_inference.is_some()
    }));
    let entry = stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::EnumEntry)
        .expect("enum entry stub");
    assert_eq!(entry.body, Some(BodyKind::EnumEntry));
    assert!(stubs.iter().any(|stub| {
        ids.anchor(stub.id)
            .is_some_and(|anchor| anchor.owner == Some(entry.id))
            && stub.signature_inference.is_some()
    }));
    assert!(stubs.iter().any(|stub| {
        stub.kind == DeclarationKind::Initializer
            && ids
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.owner == Some(entry.id))
    }));
    assert!(
        stubs
            .iter()
            .filter(|stub| stub.kind == DeclarationKind::Property)
            .count()
            >= 3
    );
}

#[test]
fn classifier_hoisted_from_enum_entry_body_keeps_the_entry_as_stable_owner() {
    let source_text = r#"
enum class Choice {
    ONE {
        val label = "one"
        inner class Inner { fun read() = label }
    }
}
"#;
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(source_text, &mut diagnostics);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let mut ids = DeclarationIds::default();
    let mut names = LookupNames::default();
    let stubs = extract_file_stubs(&file, SourceFileId::from_raw(0), &mut ids, &mut names);
    let entry = stubs
        .iter()
        .find(|stub| {
            stub.kind == DeclarationKind::EnumEntry
                && stub.lookup_name.and_then(|name| names.get(name)) == Some("ONE")
        })
        .expect("enum entry stub");
    let inner = stubs
        .iter()
        .find(|stub| {
            stub.kind == DeclarationKind::Classifier
                && stub
                    .lookup_name
                    .and_then(|name| names.get(name))
                    .and_then(|name| name.rsplit('.').next())
                    == Some("Inner")
        })
        .unwrap_or_else(|| {
            panic!(
                "hoisted inner classifier stub: {:?}",
                stubs
                    .iter()
                    .map(|stub| (
                        stub.kind,
                        stub.lookup_name.and_then(|name| names.get(name)),
                        ids.anchor(stub.id),
                    ))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(
        ids.anchor(inner.id).and_then(|anchor| anchor.owner),
        Some(entry.id)
    );
}

#[test]
fn anonymous_classifier_in_enum_entry_keeps_entry_and_nested_owner_edges() {
    let source_text = r#"
enum class Choice {
    ONE {
        val label = "one"
        val holder = object {
            inner class Inner { val value = label }
        }
    }
}
"#;
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(source_text, &mut diagnostics);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let anonymous_declaration = file
        .anonymous_object_classes
        .values()
        .copied()
        .next()
        .expect("anonymous classifier declaration");
    let anonymous_range = match file.decl(anonymous_declaration) {
        crate::ast::Decl::Class(class) => class.span,
        crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => {
            panic!("anonymous declaration must be a classifier")
        }
    };
    let inner_range = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            crate::ast::Decl::Class(class) if class.name.rsplit('.').next() == Some("Inner") => {
                Some(class.span)
            }
            crate::ast::Decl::Class(_)
            | crate::ast::Decl::Fun(_)
            | crate::ast::Decl::Property(_) => None,
        })
        .expect("nested inner classifier declaration");

    let mut ids = DeclarationIds::default();
    let mut names = LookupNames::default();
    let stubs = extract_file_stubs(&file, SourceFileId::from_raw(0), &mut ids, &mut names);
    let entry = stubs
        .iter()
        .find(|stub| {
            stub.kind == DeclarationKind::EnumEntry
                && stub.lookup_name.and_then(|name| names.get(name)) == Some("ONE")
        })
        .expect("enum entry stub");
    let anonymous = stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::Classifier && stub.range == anonymous_range)
        .expect("anonymous classifier stub");
    let inner = stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::Classifier && stub.range == inner_range)
        .expect("nested inner classifier stub");

    assert_eq!(
        ids.anchor(anonymous.id).and_then(|anchor| anchor.owner),
        Some(entry.id)
    );
    assert_eq!(
        ids.anchor(inner.id).and_then(|anchor| anchor.owner),
        Some(anonymous.id)
    );
}

#[test]
fn anonymous_classifier_in_top_level_getter_is_owned_by_the_accessor() {
    let source_text = r#"
interface Marker
val marker: Marker
    get() = object : Marker {}
"#;
    let mut diagnostics = DiagSink::new();
    let file = crate::frontend::parse_source_with_detected_features(source_text, &mut diagnostics);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let anonymous_range = file
        .anonymous_object_classes
        .values()
        .find_map(|declaration| match file.decl(*declaration) {
            crate::ast::Decl::Class(class) => Some(class.span),
            crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => None,
        })
        .expect("anonymous classifier range");

    let mut ids = DeclarationIds::default();
    let mut names = LookupNames::default();
    let stubs = extract_file_stubs(&file, SourceFileId::from_raw(0), &mut ids, &mut names);
    let getter = stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::Accessor)
        .expect("getter accessor stub");
    let anonymous = stubs
        .iter()
        .find(|stub| stub.kind == DeclarationKind::Classifier && stub.range == anonymous_range)
        .expect("anonymous classifier stub");

    assert_eq!(
        ids.anchor(anonymous.id).and_then(|anchor| anchor.owner),
        Some(getter.id)
    );
}

#[test]
fn anonymous_classifier_header_keeps_enclosing_formal_as_a_capture() {
    let inputs = [SourceInput::kotlin(
        "interface Box<T>\ninline fun <reified T> build() = object : Box<T> {}\n",
    )];
    let mut diagnostics = DiagSink::new();
    let module = stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |_, _, _| {},
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let function = module
        .stubs
        .iter()
        .find(|stub| {
            stub.kind == DeclarationKind::Function
                && stub
                    .lookup_name
                    .and_then(|name| module.lookup_names.get(name))
                    == Some("build")
        })
        .expect("enclosing function");
    let anonymous = module
        .stubs
        .iter()
        .find(|stub| stub.flags.has(DeclarationFlags::ANONYMOUS_OBJECT))
        .expect("anonymous classifier");
    assert_eq!(
        module
            .declarations
            .anchor(anonymous.id)
            .and_then(|anchor| anchor.owner),
        Some(function.id)
    );
    let HeaderDeclarationKind::Classifier {
        type_parameters,
        lexical_type_parameter_captures,
        ..
    } = module.syntax.declaration(anonymous.id).unwrap().kind
    else {
        panic!("anonymous classifier header")
    };
    assert!(module.syntax.type_parameters(type_parameters).is_empty());
    let captures = module
        .syntax
        .type_parameters(lexical_type_parameter_captures)
        .iter()
        .map(|parameter| module.lookup_names.get(parameter.name).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(captures, ["T"]);
}

#[test]
fn source_map_assigns_per_input_identities_without_retaining_source_text_or_line_offsets() {
    let mut sources = SourceMap::default();
    let id = sources.insert("src/Main.kt");
    let reparsed_id = sources.insert("src/Main.kt");
    let other_id = sources.insert("src/Other.kt");
    assert_eq!(
        sources.get(id).map(|file| file.path.as_ref()),
        Some("src/Main.kt")
    );
    assert_ne!(reparsed_id, id);
    assert_ne!(other_id, id);
    assert_eq!(sources.len(), 3);
}

#[test]
fn every_synthetic_origin_has_a_stable_source_cause() {
    let mut origins = OriginStore::default();
    let source = origins.source(SourceFileId::from_raw(2), Span::new(10, 14));
    let synthetic = origins.synthetic(source, SyntheticOriginKind::ImplicitConversion);
    assert_eq!(
        origins.get(synthetic),
        Some(Origin::Synthetic {
            cause: source,
            kind: SyntheticOriginKind::ImplicitConversion,
        })
    );
}

#[test]
fn persistent_source_map_owns_origins_without_owning_source_text() {
    let mut sources = SourceMap::default();
    let file = sources.insert("src/Main.kt");
    let source = sources.origins_mut().source(file, Span::new(3, 8));
    let synthetic = sources
        .origins_mut()
        .synthetic(source, SyntheticOriginKind::DefaultArgument);

    assert_eq!(sources.origins().len(), 2);
    assert_eq!(
        sources.origins().get(synthetic),
        Some(Origin::Synthetic {
            cause: source,
            kind: SyntheticOriginKind::DefaultArgument,
        })
    );
    assert_eq!(sources.get(file).unwrap().path.as_ref(), "src/Main.kt");
}

struct RecordingBodySink {
    accepted: Vec<(BodyOwnerId, usize)>,
}

impl CheckedBodySink for RecordingBodySink {
    fn accept_finalized(&mut self, owner: BodyOwnerId, body: FirBody) {
        assert_eq!(body.owner(), owner);
        self.accepted.push((owner, body.storage_payload_bytes()));
        // The body is consumed and dropped here. Only the compact observation survives.
    }
}

#[test]
fn body_dispatch_retains_only_semantically_inline_fir() {
    let ordinary_declaration = body_stub(DeclarationId::from_raw(3), false);
    let inline_declaration = body_stub(DeclarationId::from_raw(4), true);
    let mut index = index_with_signatures([ordinary_declaration.id, inline_declaration.id]);
    let ordinary = index.publish_function(
        CallableId::from_raw(10),
        ordinary_declaration.id,
        "ordinary",
        false,
    );
    let inline = index.publish_function(
        CallableId::from_raw(11),
        inline_declaration.id,
        "inline",
        true,
    );
    let mut inline_bodies = InlineBodyStore::default();
    let mut sink = RecordingBodySink {
        accepted: Vec::new(),
    };

    dispatch_checked_body(
        ordinary,
        body_work(ordinary_declaration),
        FirBody::new(ordinary_declaration.body_owner()),
        &mut inline_bodies,
        &mut sink,
    );
    assert_eq!(sink.accepted, vec![(ordinary_declaration.body_owner(), 0)]);
    assert!(inline_bodies.is_empty());

    dispatch_checked_body(
        inline,
        body_work(inline_declaration),
        FirBody::new(inline_declaration.body_owner()),
        &mut inline_bodies,
        &mut sink,
    );
    assert_eq!(sink.accepted.len(), 1);
    assert_eq!(inline_bodies.len(), 1);
    assert_eq!(
        inline_bodies.get(inline.id).map(FirBody::owner),
        Some(inline_declaration.body_owner())
    );
}

#[test]
fn ordinary_fir_growth_does_not_change_persistent_frontend_memory() {
    fn finish(expression_count: usize) -> (FrontendModule, usize) {
        let declaration = body_stub(DeclarationId::from_raw(3), false);
        let mut index = index_with_signatures([declaration.id]);
        let callable =
            index.publish_function(CallableId::from_raw(5), declaration.id, "body", false);
        let mut sources = SourceMap::default();
        let file = sources.insert("src/Main.kt");
        let origin = sources.origins_mut().source(file, Span::new(0, 1));
        let mut body = FirBody::new(declaration.body_owner());
        for value in 0..expression_count {
            let expression = body.add_expr(FirExpr {
                origin,
                ty: ResolvedTy::new(Ty::Int).unwrap(),
                kind: FirExprKind::Constant(FirConstant::Int(value as i64)),
            });
            let statement = body.add_statement(FirStatement {
                origin,
                kind: FirStatementKind::Expression(expression),
            });
            body.push_root(statement);
        }
        let transient_bytes = body.storage_payload_bytes();
        let mut inline_bodies = InlineBodyStore::default();
        let mut sink = RecordingBodySink {
            accepted: Vec::new(),
        };
        dispatch_checked_body(
            callable,
            body_work(declaration),
            body,
            &mut inline_bodies,
            &mut sink,
        );
        assert_eq!(sink.accepted.len(), 1);
        (
            FrontendModule::new(
                index,
                inline_bodies,
                DefaultArgumentStore::default(),
                sources,
            ),
            transient_bytes,
        )
    }

    let (small, small_transient) = finish(10);
    let (large, large_transient) = finish(100);
    assert!(large_transient > small_transient * 5);
    assert_eq!(
        large.storage_payload_bytes(),
        small.storage_payload_bytes(),
        "ordinary checked FIR must disappear at its sink boundary"
    );
    assert!(large.inline_bodies().is_empty());
}

#[test]
#[should_panic(expected = "only semantically inline declarations")]
fn inline_store_rejects_an_ordinary_resolved_callable() {
    let declaration = body_stub(DeclarationId::from_raw(8), false);
    let mut index = index_with_signatures([declaration.id]);
    let callable =
        index.publish_function(CallableId::from_raw(8), declaration.id, "ordinary", false);
    InlineBodyStore::default().insert(callable, FirBody::new(declaration.body_owner()));
}

#[test]
fn fir_nodes_can_only_embed_publishable_types_and_stable_targets() {
    let mut body = FirBody::new(BodyOwnerId::from_raw(1));
    let origin = OriginId::from_raw(2);
    let value = body.add_expr(FirExpr {
        origin,
        ty: ResolvedTy::new(Ty::Int).unwrap(),
        kind: FirExprKind::Constant(FirConstant::Int(1)),
    });
    let call = body.add_expr(FirExpr {
        origin,
        ty: ResolvedTy::new(Ty::String).unwrap(),
        kind: FirExprKind::Call(FirCall {
            target: CallableId::from_raw(9).into(),
            dispatch_receiver: None,
            extension_receiver: None,
            parameter_types: Box::new([ResolvedTy::new(Ty::Int).unwrap()]),
            arguments: Box::new([FirCallArgument::Expression {
                parameter: 0,
                value,
                conversion: None,
            }]),
            substitutions: Box::new([]),
        }),
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(call),
    });
    body.push_root(statement);

    assert_eq!(body.expr(value).unwrap().ty.get(), Ty::Int);
    assert_eq!(body.expr(call).unwrap().ty.get(), Ty::String);
    assert_eq!(body.roots(), &[statement]);
}

#[test]
fn signature_nodes_are_allocation_free_and_share_operand_storage() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<SigExpr>();

    let mut graph = SignatureGraph::default();
    let a = graph.add_expr(SigExpr::Known(ResolvedTy::new(Ty::Int).unwrap()));
    let b = graph.add_expr(SigExpr::Known(ResolvedTy::new(Ty::String).unwrap()));
    let args = graph.add_operands([a, b]);
    let scope = graph.add_scope(SignatureScope {
        owner: DeclarationId::from_raw(0),
        source: SourceFileId::from_raw(0),
    });
    let join = graph.add_expr(SigExpr::Join {
        operands: args,
        scope,
        origin: OriginId::from_raw(0),
    });

    assert_eq!(graph.operands(args), &[a, b]);
    assert_eq!(
        graph.expr(join),
        Some(SigExpr::Join {
            operands: args,
            scope,
            origin: OriginId::from_raw(0),
        })
    );
    assert_eq!(graph.node_count(), 3);
    assert_eq!(graph.operand_count(), 2);
    assert!(std::mem::size_of::<SigExpr>() <= 32);
}

#[test]
fn graph_roots_can_only_describe_inferred_signature_kinds() {
    let mut graph = SignatureGraph::default();
    let declaration = DeclarationId::from_raw(3);
    let result = graph.add_expr(SigExpr::Known(ResolvedTy::new(Ty::String).unwrap()));
    graph.add_inferred_constraint(
        &inferred_stub(declaration, InferredSignatureKind::ExpressionFunction),
        result,
        OriginId::from_raw(0),
    );
    assert_eq!(
        graph.constraints(),
        &[SignatureConstraint {
            declaration,
            result,
            kind: InferredSignatureKind::ExpressionFunction,
            origin: OriginId::from_raw(0),
        }]
    );
}

#[test]
#[should_panic(expected = "requires a pending-free published signature")]
fn callable_identity_cannot_precede_signature_finalization() {
    ResolvedModuleIndex::default().publish_function(
        CallableId::from_raw(1),
        DeclarationId::from_raw(1),
        "missing",
        false,
    );
}

#[test]
#[should_panic(expected = "only an inferred declaration")]
fn explicit_declaration_cannot_enter_the_signature_graph() {
    let mut graph = SignatureGraph::default();
    let result = graph.add_expr(SigExpr::Known(ResolvedTy::new(Ty::String).unwrap()));
    let mut explicit = inferred_stub(
        DeclarationId::from_raw(8),
        InferredSignatureKind::ExpressionFunction,
    );
    explicit.signature_inference = None;
    graph.add_inferred_constraint(&explicit, result, OriginId::from_raw(0));
}

#[test]
fn resolved_signatures_reject_pending_and_error_at_construction() {
    assert_eq!(
        ResolvedSignature::new([], Ty::Pending),
        Err(UnpublishableType::Pending)
    );
    assert_eq!(
        ResolvedSignature::new([Ty::Int], Ty::Error),
        Err(UnpublishableType::Error)
    );
    assert_eq!(
        ResolvedSignature::new([Ty::nullable(Ty::Pending)], Ty::Unit),
        Err(UnpublishableType::Pending)
    );
}

#[test]
fn finalization_consumes_lazy_state_and_requires_every_signature() {
    let a = DeclarationId::from_raw(0);
    let b = DeclarationId::from_raw(1);
    let signature = ResolvedSignature::new([Ty::Int], Ty::String).unwrap();
    let states = HashMap::from([
        (a, SignatureState::Resolved(signature.clone())),
        (b, SignatureState::Computing),
    ]);
    assert_eq!(
        finalize_signatures([a, b], states),
        Err(SignatureFinalizationError {
            declarations: vec![(b, None)],
        })
    );

    let index = finalize_signatures(
        [a],
        HashMap::from([(a, SignatureState::Resolved(signature.clone()))]),
    )
    .unwrap();
    assert_eq!(index.signature(a), Some(&signature));
}

struct TestSignatureSemantics {
    evaluations: Cell<u32>,
    operations: RefCell<Vec<String>>,
    call_dependency: Option<DeclarationId>,
}

impl TestSignatureSemantics {
    fn new() -> Self {
        Self {
            evaluations: Cell::new(0),
            operations: RefCell::new(Vec::new()),
            call_dependency: None,
        }
    }

    fn with_call_dependency(declaration: DeclarationId) -> Self {
        Self {
            call_dependency: Some(declaration),
            ..Self::new()
        }
    }
}

impl SignatureSemantics for TestSignatureSemantics {
    fn classifier_type(
        &self,
        _declaration: DeclarationId,
        _scope: SignatureScope,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("classifier-type".into());
        Ok(ResolvedTy::new(Ty::obj("TestClassifier")).unwrap())
    }

    fn declaration_parameters(
        &self,
        _declaration: DeclarationId,
    ) -> Result<Box<[ResolvedTy]>, DiagnosticId> {
        self.evaluations.set(self.evaluations.get() + 1);
        self.operations.borrow_mut().push("parameters".into());
        Ok(Box::new([]))
    }

    fn resolve_type(
        &self,
        _scope: SignatureScope,
        _origin: OriginId,
        syntax: HeaderTypeId,
        graph: &SignatureGraph,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("type".into());
        let reference = graph
            .transient_type_ref(syntax)
            .ok_or_else(|| DiagnosticId::from_raw(2_004))?;
        ResolvedTy::new(Ty::from_name(&reference.name).unwrap_or(Ty::String))
            .map_err(|_| DiagnosticId::from_raw(2_004))
    }

    fn resolve_contextual_type(
        &self,
        scope: SignatureScope,
        origin: OriginId,
        syntax: HeaderTypeId,
        _expected: ResolvedTy,
        graph: &SignatureGraph,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.resolve_type(scope, origin, syntax, graph)
    }

    fn select_value(
        &self,
        _scope: SignatureScope,
        spelling: &str,
        _origin: OriginId,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push(format!("value:{spelling}"));
        if let Some(declaration) = self.call_dependency {
            return demand(declaration).map(|signature| signature.result);
        }
        expected.ok_or_else(|| DiagnosticId::from_raw(2_001))
    }

    fn select_call(
        &self,
        _scope: SignatureScope,
        spelling: &str,
        _origin: OriginId,
        arguments: &[ResolvedSigCallArgument<'_>],
        _type_arguments: &[ResolvedTy],
        _trailing_lambda: bool,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push(format!("call:{spelling}"));
        if let Some(declaration) = self.call_dependency {
            return demand(declaration).map(|signature| signature.result);
        }
        Ok(expected
            .or_else(|| arguments.first().map(|argument| argument.ty))
            .unwrap())
    }

    fn call_argument_expectations(
        &self,
        _scope: SignatureScope,
        _spelling: &str,
        _origin: OriginId,
        arguments: &[SigCallArgumentProbe<'_>],
        _type_arguments: &[ResolvedTy],
        _trailing_lambda: bool,
        _expected: Option<ResolvedTy>,
        _demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<Box<[Option<ResolvedTy>]>, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push("call-expectations".into());
        Ok(vec![None; arguments.len()].into_boxed_slice())
    }

    fn select_callable_reference(
        &self,
        _scope: SignatureScope,
        spelling: &str,
        _origin: OriginId,
        expected: Option<ResolvedTy>,
        demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push(format!("callable-reference:{spelling}"));
        if let Some(declaration) = self.call_dependency {
            return demand(declaration).map(|signature| signature.result);
        }
        expected.ok_or_else(|| DiagnosticId::from_raw(2_001))
    }

    fn select_bound_callable_reference(
        &self,
        _scope: SignatureScope,
        spelling: &str,
        _origin: OriginId,
        receiver: ResolvedTy,
        _unbound: bool,
        expected: Option<ResolvedTy>,
        _demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push(format!("bound-callable-reference:{spelling}"));
        Ok(expected.unwrap_or(receiver))
    }

    fn select_lateinit_initialized(
        &self,
        _scope: SignatureScope,
        spelling: &str,
        _origin: OriginId,
        _receiver: Option<ResolvedTy>,
        _unbound: bool,
        _demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push(format!("lateinit-initialized:{spelling}"));
        ResolvedTy::new(Ty::Boolean).map_err(|_| DiagnosticId::from_raw(2_001))
    }

    fn class_literal_type(
        &self,
        receiver: ResolvedTy,
        unbound: bool,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push(
            if unbound {
                "unbound-class-literal"
            } else {
                "bound-class-literal"
            }
            .into(),
        );
        Ok(receiver)
    }

    fn class_literal_receiver_is_value(
        &self,
        _scope: SignatureScope,
        _root: &str,
    ) -> Result<bool, DiagnosticId> {
        Ok(false)
    }

    fn select_member(
        &self,
        _scope: SignatureScope,
        spelling: &str,
        _origin: OriginId,
        receiver: ResolvedTy,
        expected: Option<ResolvedTy>,
        _demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push(format!("member:{spelling}"));
        Ok(expected.unwrap_or(receiver))
    }

    fn select_member_call(
        &self,
        _scope: SignatureScope,
        spelling: &str,
        _origin: OriginId,
        receiver: ResolvedTy,
        arguments: &[ResolvedSigCallArgument<'_>],
        _type_arguments: &[ResolvedTy],
        _trailing_lambda: bool,
        expected: Option<ResolvedTy>,
        _demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedMemberCall, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push(format!("member-call:{spelling}"));
        Ok(ResolvedMemberCall {
            ty: Some(
                expected
                    .or_else(|| arguments.last().map(|argument| argument.ty))
                    .unwrap_or(receiver),
            ),
            declaration: None,
        })
    }

    fn member_call_argument_expectations(
        &self,
        _scope: SignatureScope,
        _spelling: &str,
        _origin: OriginId,
        _receiver: ResolvedTy,
        arguments: &[SigCallArgumentProbe<'_>],
        _type_arguments: &[ResolvedTy],
        _trailing_lambda: bool,
        _expected: Option<ResolvedTy>,
        _demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<Box<[Option<ResolvedTy>]>, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push("member-call-expectations".into());
        Ok(vec![None; arguments.len()].into_boxed_slice())
    }

    fn select_binary(
        &self,
        _scope: SignatureScope,
        _operator: SigBinaryOperator,
        _origin: OriginId,
        lhs: ResolvedTy,
        rhs: ResolvedTy,
        _demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("binary".into());
        assert_eq!(lhs, rhs);
        Ok(lhs)
    }

    fn select_invoke(
        &self,
        _scope: SignatureScope,
        _origin: OriginId,
        callee: ResolvedTy,
        arguments: &[ResolvedSigCallArgument<'_>],
        _demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("invoke".into());
        Ok(arguments
            .last()
            .map(|argument| argument.ty)
            .unwrap_or(callee))
    }

    fn invoke_argument_expectations(
        &self,
        _scope: SignatureScope,
        callee: ResolvedTy,
        arguments: &[SigCallArgumentProbe<'_>],
    ) -> Result<Box<[Option<ResolvedTy>]>, DiagnosticId> {
        self.operations
            .borrow_mut()
            .push("invoke-expectations".into());
        let Ty::Fun(signature) = callee.get().non_null() else {
            return Ok(vec![None; arguments.len()].into_boxed_slice());
        };
        if signature.params.len() != arguments.len() {
            return Ok(vec![None; arguments.len()].into_boxed_slice());
        }
        signature
            .params
            .iter()
            .copied()
            .map(|parameter| {
                ResolvedTy::new(parameter)
                    .map(Some)
                    .map_err(|_| DiagnosticId::from_raw(2_004))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Vec::into_boxed_slice)
    }

    fn make_function_type(
        &self,
        parameters: &[ResolvedTy],
        result: ResolvedTy,
        context_count: u32,
        has_receiver: bool,
        suspend: bool,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("function".into());
        ResolvedTy::new(Ty::fun_with_shape(
            parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect::<Vec<_>>(),
            result.get(),
            context_count as usize,
            has_receiver,
            suspend,
        ))
        .map_err(|_| DiagnosticId::from_raw(2_003))
    }

    fn select_delegate(
        &self,
        _declaration: DeclarationId,
        _scope: SignatureScope,
        _origin: OriginId,
        delegate: ResolvedTy,
        _local: bool,
        _demand: &mut dyn FnMut(DeclarationId) -> Result<ResolvedSignature, DiagnosticId>,
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("delegate".into());
        Ok(delegate)
    }

    fn least_upper_bound(
        &self,
        _scope: SignatureScope,
        _origin: OriginId,
        operands: &[ResolvedTy],
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("join".into());
        let first = *operands.first().expect("test join must not be empty");
        assert!(operands.iter().all(|operand| *operand == first));
        Ok(first)
    }

    fn make_nullable(&self, base: ResolvedTy) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("nullable".into());
        ResolvedTy::new(Ty::nullable(base.get())).map_err(|_| DiagnosticId::from_raw(2_000))
    }

    fn make_non_nullable(&self, base: ResolvedTy) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("non-nullable".into());
        ResolvedTy::new(base.get().non_null()).map_err(|_| DiagnosticId::from_raw(2_002))
    }

    fn substitute(
        &self,
        base: ResolvedTy,
        substitutions: &[(TypeParameterId, ResolvedTy)],
    ) -> Result<ResolvedTy, DiagnosticId> {
        self.operations.borrow_mut().push("substitute".into());
        Ok(substitutions.last().map_or(base, |(_, value)| *value))
    }

    fn recursive_inference_diagnostic(&self, declaration: DeclarationId) -> DiagnosticId {
        DiagnosticId::from_raw(100 + declaration.raw())
    }

    fn missing_signature_diagnostic(&self, declaration: DeclarationId) -> DiagnosticId {
        DiagnosticId::from_raw(1_000 + declaration.raw())
    }
}

#[test]
fn resolver_selection_demands_the_selected_declaration_through_the_solver() {
    let caller = DeclarationId::from_raw(30);
    let selected = DeclarationId::from_raw(31);
    let origin = OriginId::from_raw(3);
    let mut graph = SignatureGraph::default();
    let scope = graph.add_scope(SignatureScope {
        owner: caller,
        source: SourceFileId::from_raw(0),
    });
    let spelling = graph.intern_name("selected");
    let target = graph.add_callable_selection(DeferredCallableSelection {
        scope,
        spelling,
        origin,
        expected: None,
        type_arguments: OperandRange::default(),
        trailing_lambda: false,
    });
    let arguments = graph.add_call_arguments([]);
    let call = graph.add_expr(SigExpr::Call { target, arguments });
    let selected_result = graph.add_expr(SigExpr::Known(
        ResolvedTy::new(Ty::String).expect("String is publishable"),
    ));
    graph.add_inferred_constraint(
        &inferred_stub(caller, InferredSignatureKind::ExpressionFunction),
        call,
        OriginId::from_raw(0),
    );
    graph.add_inferred_constraint(
        &inferred_stub(selected, InferredSignatureKind::ExpressionFunction),
        selected_result,
        OriginId::from_raw(0),
    );

    let semantics = TestSignatureSemantics::with_call_dependency(selected);
    let index = SignatureSolver::new(graph, [caller, selected])
        .finalize(&ResolverBackedSignatureEvaluator::new(&semantics))
        .unwrap();

    assert_eq!(index.signature(caller).unwrap().result.get(), Ty::String);
    assert_eq!(semantics.evaluations.get(), 2);
    assert_eq!(
        semantics.operations.into_inner(),
        [
            "call-expectations",
            "call:selected",
            "parameters",
            "parameters"
        ]
    );
}

#[test]
fn compact_graph_walker_delegates_every_semantic_operation() {
    let declaration = DeclarationId::from_raw(20);
    let origin = OriginId::from_raw(7);
    let mut graph = SignatureGraph::default();
    let scope = graph.add_scope(SignatureScope {
        owner: declaration,
        source: SourceFileId::from_raw(0),
    });
    let function_name = graph.intern_name("produce");
    let member_name = graph.intern_name("value");
    let int = graph.add_expr(SigExpr::Known(ResolvedTy::new(Ty::Int).unwrap()));
    let string = graph.add_expr(SigExpr::Known(ResolvedTy::new(Ty::String).unwrap()));
    let call_selection = graph.add_callable_selection(DeferredCallableSelection {
        scope,
        spelling: function_name,
        origin,
        expected: Some(string),
        type_arguments: OperandRange::default(),
        trailing_lambda: false,
    });
    let call_arguments = graph.add_call_arguments([SigCallArgument {
        value: int,
        name: None,
        spread: false,
        integer_literal: None,
        lambda: false,
    }]);
    let call = graph.add_expr(SigExpr::Call {
        target: call_selection,
        arguments: call_arguments,
    });
    let member_selection = graph.add_member_selection(DeferredMemberSelection {
        scope,
        spelling: member_name,
        origin,
        expected: None,
        type_arguments: OperandRange::default(),
        trailing_lambda: false,
    });
    let member = graph.add_expr(SigExpr::Member {
        receiver: call,
        lookup: member_selection,
        origin,
    });
    let member_call_arguments = graph.add_call_arguments([SigCallArgument {
        value: string,
        name: None,
        spread: false,
        integer_literal: None,
        lambda: false,
    }]);
    let member_call = graph.add_expr(SigExpr::MemberCall {
        receiver: string,
        target: member_selection,
        arguments: member_call_arguments,
        origin,
    });
    let invoke_arguments = graph.add_call_arguments([SigCallArgument {
        value: int,
        name: None,
        spread: false,
        integer_literal: None,
        lambda: false,
    }]);
    let invoke = graph.add_expr(SigExpr::Invoke {
        callee: member,
        arguments: invoke_arguments,
        scope,
        origin,
    });
    let nullable = graph.add_expr(SigExpr::Nullable(invoke));
    let substitutions = graph.add_substitutions([SigSubstitution {
        parameter: TypeParameterId::from_raw(0).into(),
        value: string,
    }]);
    let substituted = graph.add_expr(SigExpr::Substitute {
        base: nullable,
        substitutions,
    });
    let binary = graph.add_expr(SigExpr::Binary {
        operator: SigBinaryOperator::Add,
        lhs: string,
        rhs: string,
        scope,
        origin,
    });
    let callable_reference = graph.add_expr(SigExpr::CallableReference(call_selection));
    let bound_callable_reference = graph.add_expr(SigExpr::BoundCallableReference {
        receiver: string,
        classifier: None,
        scope,
        root: None,
        target: call_selection,
    });
    let joined_operands = graph.add_operands([
        string,
        substituted,
        binary,
        callable_reference,
        bound_callable_reference,
        member_call,
    ]);
    let joined = graph.add_expr(SigExpr::Join {
        operands: joined_operands,
        scope,
        origin,
    });
    graph.add_inferred_constraint(
        &inferred_stub(declaration, InferredSignatureKind::ExpressionFunction),
        joined,
        OriginId::from_raw(0),
    );

    let semantics = TestSignatureSemantics::new();
    let index = SignatureSolver::new(graph, [declaration])
        .finalize(&ResolverBackedSignatureEvaluator::new(&semantics))
        .unwrap();
    assert_eq!(
        index.signature(declaration).unwrap().result.get(),
        Ty::String
    );
    let mut expected_operations = vec![
        "call-expectations",
        "call:produce",
        "member:value",
        "invoke-expectations",
        "invoke",
        "nullable",
        "substitute",
        "binary",
        "callable-reference:produce",
        "bound-callable-reference:produce",
        "member-call-expectations",
        "member-call:value",
    ];
    // The six-way join asks the semantic LUB operation twice per operand while testing whether a
    // branch can be contextually rebound, then once for the final result.
    expected_operations.extend(vec!["join"; 13]);
    expected_operations.push("parameters");
    assert_eq!(semantics.operations.into_inner(), expected_operations);
}

#[test]
fn signature_solver_demands_dependencies_once_and_memoizes_the_answer() {
    let a = DeclarationId::from_raw(0);
    let b = DeclarationId::from_raw(1);
    let mut graph = SignatureGraph::default();
    let a_result = graph.add_expr(SigExpr::DeclarationType(b));
    let b_result = graph.add_expr(SigExpr::Known(ResolvedTy::new(Ty::String).unwrap()));
    graph.add_inferred_constraint(
        &inferred_stub(a, InferredSignatureKind::ExpressionFunction),
        a_result,
        OriginId::from_raw(0),
    );
    graph.add_inferred_constraint(
        &inferred_stub(b, InferredSignatureKind::ExpressionFunction),
        b_result,
        OriginId::from_raw(0),
    );

    let semantics = TestSignatureSemantics::new();
    let evaluator = ResolverBackedSignatureEvaluator::new(&semantics);
    let mut solver = SignatureSolver::new(graph, [a, b]);
    assert_eq!(
        solver.resolve(a, &evaluator).unwrap().result.get(),
        Ty::String
    );
    assert_eq!(
        solver.resolve(a, &evaluator).unwrap().result.get(),
        Ty::String
    );
    assert_eq!(semantics.evaluations.get(), 2);

    let index = solver.finalize(&evaluator).unwrap();
    assert_eq!(index.signature(a).unwrap().result.get(), Ty::String);
    assert_eq!(index.signature(b).unwrap().result.get(), Ty::String);
    assert_eq!(semantics.evaluations.get(), 2);
}

#[test]
fn signature_solver_marks_every_member_of_an_unanchored_cycle_failed() {
    let a = DeclarationId::from_raw(4);
    let b = DeclarationId::from_raw(7);
    let mut graph = SignatureGraph::default();
    let a_result = graph.add_expr(SigExpr::DeclarationType(b));
    let b_result = graph.add_expr(SigExpr::DeclarationType(a));
    graph.add_inferred_constraint(
        &inferred_stub(a, InferredSignatureKind::ExpressionFunction),
        a_result,
        OriginId::from_raw(0),
    );
    graph.add_inferred_constraint(
        &inferred_stub(b, InferredSignatureKind::ExpressionFunction),
        b_result,
        OriginId::from_raw(0),
    );

    let semantics = TestSignatureSemantics::new();
    let evaluator = ResolverBackedSignatureEvaluator::new(&semantics);
    let mut solver = SignatureSolver::new(graph, [a, b]);
    assert_eq!(
        solver.resolve(a, &evaluator),
        Err(DiagnosticId::from_raw(104))
    );
    assert_eq!(
        solver.state(a),
        Some(&SignatureState::Failed(DiagnosticId::from_raw(104)))
    );
    assert_eq!(
        solver.state(b),
        Some(&SignatureState::Failed(DiagnosticId::from_raw(107)))
    );
    assert_eq!(
        solver.finalize(&evaluator),
        Err(SignatureFinalizationError {
            declarations: vec![
                (a, Some(DiagnosticId::from_raw(104))),
                (b, Some(DiagnosticId::from_raw(107))),
            ],
        })
    );
}

#[test]
fn missing_required_signature_fails_with_an_owned_diagnostic() {
    let declaration = DeclarationId::from_raw(9);
    let semantics = TestSignatureSemantics::new();
    let evaluator = ResolverBackedSignatureEvaluator::new(&semantics);
    let solver = SignatureSolver::new(SignatureGraph::default(), [declaration]);
    assert_eq!(
        solver.finalize(&evaluator),
        Err(SignatureFinalizationError {
            declarations: vec![(declaration, Some(DiagnosticId::from_raw(1_009)))],
        })
    );
}

#[test]
fn larger_inferred_expression_lives_only_in_the_temporary_graph() {
    fn solve_with_leaf_count(leaves: usize) -> (usize, ResolvedModuleIndex) {
        let declaration = DeclarationId::from_raw(0);
        let mut graph = SignatureGraph::default();
        let leaves = (0..leaves)
            .map(|_| graph.add_expr(SigExpr::Known(ResolvedTy::new(Ty::String).unwrap())))
            .collect::<Vec<_>>();
        let operands = graph.add_operands(leaves);
        let scope = graph.add_scope(SignatureScope {
            owner: declaration,
            source: SourceFileId::from_raw(0),
        });
        let result = graph.add_expr(SigExpr::Join {
            operands,
            scope,
            origin: OriginId::from_raw(0),
        });
        graph.add_inferred_constraint(
            &inferred_stub(declaration, InferredSignatureKind::PropertyInitializer),
            result,
            OriginId::from_raw(0),
        );
        let temporary_bytes = graph.storage_payload_bytes();
        let semantics = TestSignatureSemantics::new();
        let index = SignatureSolver::new(graph, [declaration])
            .finalize(&ResolverBackedSignatureEvaluator::new(&semantics))
            .unwrap();
        (temporary_bytes, index)
    }

    let (small_temporary, small_index) = solve_with_leaf_count(10);
    let (large_temporary, large_index) = solve_with_leaf_count(100);
    assert!(large_temporary > small_temporary * 5);
    assert_eq!(
        large_index.storage_payload_bytes(),
        small_index.storage_payload_bytes(),
        "signature-expression growth must disappear with the consumed graph"
    );
}
