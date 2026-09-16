use super::test_support::{checked_function_body, root_expression};
use super::*;

#[derive(Default)]
struct StreamedBodySink(Vec<FirBody>);

impl CheckedBodySink for StreamedBodySink {
    fn accept_finalized(&mut self, _owner: BodyOwnerId, body: FirBody) {
        self.0.push(body);
    }
}

fn checked_streamed_bodies(source: &str) -> (ResolvedModuleIndex, Vec<FirBody>) {
    let inputs = [crate::source::SourceInput::kotlin(source).with_file_stem("FirLocalCapture")];
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(crate::libraries::EmptySymbolSource),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _defaults, mut sources) = streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &analysis.symbols,
        info,
        &mut index,
    )
    .expect("local signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = StreamedBodySink::default();
    for work in ordinary {
        super::driver::check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("local bodies must become checked FIR");
    }
    (index, sink.0)
}

fn production_frontend_ok(source: &str) {
    let inputs = [crate::source::SourceInput::kotlin(source).with_file_stem("FirLocalClass")];
    let mut diagnostics = crate::diag::DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        Box::new(crate::libraries::EmptySymbolSource),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn anonymous_interface_delegate_is_a_checked_constructor_argument() {
    let (body, index) = checked_function_body(
        "interface A { fun value(): String }\n\
         class Impl : A { override fun value(): String = \"OK\" }\n\
         fun box(impl: Impl): String {\n\
             val delegated = object : A by impl {}\n\
             return delegated.value()\n\
         }\n",
        "box",
    );
    let object = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::AnonymousObject(object) = &expression.kind else {
                return None;
            };
            Some(object)
        })
        .expect("anonymous object FIR");
    assert!(object.captures.is_empty());
    let [argument] = object.delegate_arguments.as_ref() else {
        panic!("one checked delegate argument expected: {object:?}")
    };
    assert_eq!(argument.delegation, 0);
    assert!(matches!(
        body.expr(argument.value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(_))
    ));
    let [delegation] = index
        .classifier_header(object.declaration)
        .expect("anonymous classifier plan")
        .interface_delegations
        .as_ref()
    else {
        panic!("one resolved delegation expected")
    };
    assert_eq!(
        delegation.source,
        crate::fir::ResolvedInterfaceDelegateSource::SyntheticConstructorParameter(0)
    );
}

#[test]
fn inferred_intersection_delegate_carries_its_selected_interface_projection() {
    let (body, _) = checked_function_body(
        "interface A\n\
         interface B { fun value(): String }\n\
         abstract class C : A, B\n\
         abstract class D : A, B\n\
         fun <T> select(left: T, right: T): T = left\n\
         fun box(c: C, d: D): String {\n\
             val intersection = select(c, d)\n\
             val delegated = object : B by intersection {}\n\
             return \"OK\"\n\
         }\n",
        "box",
    );
    let object = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::AnonymousObject(object) = &expression.kind else {
                return None;
            };
            Some(object)
        })
        .expect("anonymous object FIR");
    let [argument] = object.delegate_arguments.as_ref() else {
        panic!("one checked delegate argument expected: {object:?}")
    };
    let conversion = body.expr(argument.value).expect("converted delegate value");
    assert_eq!(conversion.ty.get(), Ty::obj("B"));
    assert!(matches!(
        &conversion.kind,
        FirExprKind::ImplicitConversion {
            conversion: FirConversion {
                kind: FirConversionKind::SmartCast { to },
                ..
            },
            ..
        } if to.get() == Ty::obj("B")
    ));
}

#[test]
fn local_class_declaration_keeps_stable_identity_and_ordered_capture_sources() {
    let (body, index) = checked_function_body(
        "fun box(): String {\n\
             val prefix = \"O\"\n\
             class Local { fun read(): String = prefix + \"K\" }\n\
             return Local().read()\n\
         }\n",
        "box",
    );
    let FirExprKind::Block { statements, .. } = &body
        .expr(root_expression(&body))
        .expect("function block")
        .kind
    else {
        panic!("local class must remain in its enclosing checked block")
    };
    let (declaration, captures) = statements
        .iter()
        .filter_map(|statement| body.statement(*statement))
        .find_map(|statement| match &statement.kind {
            FirStatementKind::LocalDeclaration {
                declaration,
                captures,
            } => Some((*declaration, captures.as_ref())),
            _ => None,
        })
        .expect("local class declaration FIR");
    assert!(index
        .declaration_header(declaration)
        .expect("local classifier header")
        .flags
        .has(crate::fir::DeclarationFlags::LOCAL_CLASS));
    let [capture] = captures else {
        panic!("local class must retain its one lexical capture")
    };
    assert_eq!(capture.name.as_ref(), "prefix");
    assert_eq!(capture.ty.get(), Ty::String);
    assert!(matches!(
        &capture.source,
        FirLocalClassCaptureSource::Value(_)
    ));
}

/// The read's coordinate says which of the two lookups lowering must perform, and it has to survive
/// every frame between the constructor prefix and the body that reads it. A read in the constructor
/// body itself is the parameter; inside a lambda there it is that frame's capture, registered at the
/// depth of the body that supplies it — which GROWS with the nesting, so the depth is not a constant
/// a lookup may leave out. Getting this wrong is silent: every spelling produces a value of the
/// right type, and all but one of them is another declaration's.
#[test]
fn a_constructor_prefix_read_carries_its_frame_in_fir() {
    for (frames, source, expected) in [
        (
            "in the constructor body itself",
            "open class Base(val text: String)\n\
             fun box(): String {\n\
             val o = \"O\"\n\
             class Local(k: String) : Base(o + k)\n\
             return Local(\"K\").text }\n",
            crate::fir::FirConstructorCaptureSite::Parameter,
        ),
        (
            "one lambda deep",
            "open class Base(val fn: () -> String)\n\
             fun box(): String {\n\
             val o = \"O\"\n\
             class Local(k: String) : Base({ o + k })\n\
             return Local(\"K\").fn() }\n",
            crate::fir::FirConstructorCaptureSite::Captured { enclosing_depth: 0 },
        ),
        (
            "two lambdas deep",
            "open class Base(val fn: () -> String)\n\
             fun box(): String {\n\
             val o = \"O\"\n\
             class Local(k: String) : Base({ { o + k }() })\n\
             return Local(\"K\").fn() }\n",
            crate::fir::FirConstructorCaptureSite::Captured { enclosing_depth: 1 },
        ),
    ] {
        let (_, bodies) = checked_streamed_bodies(source);
        // A lambda's body is its own arena hanging off the expression that creates it, so a walk of
        // the outer bodies alone would see a prefix read only in the constructor body — which is
        // the one case out of three where the coordinate is `Parameter`.
        fn sites_in(body: &FirBody, into: &mut Vec<crate::fir::FirConstructorCaptureSite>) {
            for raw in 0..body.expression_count() {
                let Some(expression) = body.expr(FirExprId::from_raw(raw as u32)) else {
                    continue;
                };
                match &expression.kind {
                    FirExprKind::ConstructorCaptureRead { site, .. } => into.push(*site),
                    // Lambdas are the only frames worth descending into: every other nested body
                    // clears the prefix rule, so a prefix read cannot appear below one.
                    FirExprKind::Lambda { body: nested, .. } => sites_in(nested, into),
                    _ => {}
                }
            }
        }
        let mut sites = Vec::new();
        for body in &bodies {
            sites_in(body, &mut sites);
        }
        assert_eq!(
            sites.as_slice(),
            &[expected],
            "a prefix read {frames} must carry that coordinate"
        );
    }
}

#[test]
fn anonymous_super_argument_carries_the_constructor_prefix_capture_in_fir() {
    let (index, bodies) = checked_streamed_bodies(
        "interface Callback { fun invoke(): String }\n\
         open class Base(val fn: Callback)\n\
         fun box(): String {\n\
             val ok = \"OK\"\n\
             class Local : Base(object : Callback { override fun invoke() = ok })\n\
             return Local().fn.invoke()\n\
         }\n",
    );
    let captures = bodies
        .iter()
        .flat_map(|body| {
            (0..body.expression_count()).filter_map(move |raw| {
                let FirExprKind::AnonymousObject(object) =
                    &body.expr(FirExprId::from_raw(raw as u32))?.kind
                else {
                    return None;
                };
                object.captures.first().map(|capture| (body, capture))
            })
        })
        .collect::<Vec<_>>();
    let [(body, capture)] = captures.as_slice() else {
        panic!("one anonymous super-argument capture expected: {captures:?}")
    };
    let FirLocalClassCaptureSource::ConstructorCapture { owner, field, site } = &capture.source
    else {
        panic!("constructor-prefix capture expected: {capture:?}")
    };
    // The construction happens in the constructor body itself, so the prefix parameter is in
    // scope and the coordinate says so — a capture coordinate here would send lowering looking
    // for a slot that does not exist.
    assert_eq!(*site, crate::fir::FirConstructorCaptureSite::Parameter);
    let constructor = DeclarationId::from_raw(body.owner().raw());
    let anchor = index
        .declaration_anchor(constructor)
        .expect("capturing body must retain its constructor declaration");
    assert_eq!(anchor.kind, DeclarationKind::Constructor);
    assert_eq!(anchor.owner, Some(*owner));
    assert_eq!(*field, 0);
}

#[test]
fn inferred_body_local_member_extension_is_selected_from_its_dispatch_rung() {
    production_frontend_ok(
        "fun box(): String {\n\
             class Local {\n\
                 fun String.decorate() = this + \"K\"\n\
                 fun result() = \"O\".decorate()\n\
             }\n\
             return Local().result()\n\
         }\n",
    );
}

#[test]
fn inferred_body_local_member_overloads_are_selected_before_forward_dependency_checking() {
    production_frontend_ok(
        "fun box(): String {\n\
             class Local {\n\
                 fun result() = choose(\"OK\")\n\
                 fun choose(value: Int) = \"FAIL$value\"\n\
                 fun choose(value: String) = value\n\
             }\n\
             return Local().result()\n\
         }\n",
    );
}

#[test]
fn body_local_member_callable_reference_uses_the_same_member_rung_as_a_call() {
    production_frontend_ok(
        "fun box(): String {\n\
             class Local { fun foo() = \"OK\" }\n\
             val reference = Local::foo\n\
             return reference(Local())\n\
         }\n",
    );
}

#[test]
fn anonymous_object_member_reads_its_already_typed_body_property() {
    production_frontend_ok(
        "fun box(): String {\n\
             val value = object {\n\
                 var index = 0\n\
                 fun next() = index++\n\
             }\n\
             return \"OK\"\n\
         }\n",
    );
}

#[test]
fn nested_default_preparation_does_not_own_the_enclosing_ordinary_expression() {
    production_frontend_ok(
        "fun <R> run(block: () -> R): R = block()\n\
         fun box(): String {\n\
             return run {\n\
                 open class A {\n\
                     open fun foo(x: String, y: String? = null): String = x + (y ?: \"K\")\n\
                 }\n\
                 class B : A() {\n\
                     override fun foo(x: String, y: String?) = super.foo(x, y)\n\
                 }\n\
                 B()\n\
             }.foo(\"O\")\n\
         }\n",
    );
}

#[test]
fn anonymous_object_outer_capture_publishes_exact_inner_receiver_path() {
    let (body, index) = checked_function_body(
        "class Test {\n\
             val content = 1\n\
             inner class A {\n\
                 fun make(): Any = object { fun read(): Int = content }\n\
             }\n\
         }\n",
        "make",
    );

    let capture = (0..body.expression_count())
        .find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            let FirExprKind::AnonymousObject(object) = &expression.kind else {
                return None;
            };
            object.captures.first()
        })
        .expect("anonymous object outer capture");
    assert_eq!(capture.ty.get(), Ty::obj("Test"));
    let FirLocalClassCaptureSource::EnclosingReceiver { path } = &capture.source else {
        panic!("outer capture must carry its checked enclosing path: {capture:?}")
    };
    let [inner] = path.as_ref() else {
        panic!("Test is one enclosing-instance edge from Test.A: {path:?}")
    };
    assert!(index
        .classifier_header(*inner)
        .is_some_and(|classifier| classifier.classifier.matches("Test$A")));
}

#[test]
fn anonymous_object_inside_lambda_publishes_lifted_outer_receiver_capture() {
    let (body, _) = checked_function_body(
        "fun execute(block: () -> Unit) { block() }\n\
         class Outer(val value: String) {\n\
             fun build() {\n\
                 execute { val ignored = object { val captured = value } }\n\
             }\n\
         }\n",
        "build",
    );
    let lambda = (0..body.expression_count())
        .find_map(
            |raw| match &body.expr(FirExprId::from_raw(raw as u32))?.kind {
                FirExprKind::Lambda { body, .. } => Some(body.as_ref()),
                _ => None,
            },
        )
        .expect("nested lambda body");
    let receiver = lambda
        .implicit_receiver_captures()
        .first()
        .expect("outer dispatch receiver must be lifted into the lambda");
    assert!(receiver
        .ty
        .get()
        .obj_internal()
        .is_some_and(|ty| ty.matches("Outer")));
    let capture = (0..lambda.expression_count())
        .find_map(|raw| {
            let FirExprKind::AnonymousObject(object) =
                &lambda.expr(FirExprId::from_raw(raw as u32))?.kind
            else {
                return None;
            };
            object.captures.first()
        })
        .expect("anonymous object receiver capture");
    assert!(matches!(
        &capture.source,
        FirLocalClassCaptureSource::CapturedImplicitReceiver {
            enclosing_depth: 0,
            current: true,
            depth: 0,
            path,
        } if path.is_empty()
    ));
}

#[test]
fn local_class_member_resolves_its_lexically_nested_typealias_constructor() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +NestedTypeAliases +LocalTypeAliases\n\
         fun box(): String {\n\
             class Local {\n\
                 val value = \"OK\"\n\
                 typealias Alias = Local\n\
                 fun read(): String = Alias().value\n\
             }\n\
             return Local().read()\n\
         }\n",
        "read",
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::ConstructorCall(_))
        )
    }));
}

#[test]
fn local_class_member_alias_survives_preceding_local_classifier() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +NestedTypeAliases +LocalTypeAliases\n\
         fun box(): String {\n\
             class Earlier<T>(val value: T)\n\
             open class Local {\n\
                 val value: String get() = \"OK\"\n\
                 typealias Alias = Local\n\
                 typealias GenericAlias<T> = Earlier<T>\n\
                 fun read(): String = Alias().value\n\
             }\n\
             return Local().read()\n\
         }\n",
        "read",
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::ConstructorCall(_))
        )
    }));
}

#[test]
fn statement_local_generic_alias_preserves_inner_constructor_arguments() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +NestedTypeAliases +LocalTypeAliases\n\
         class Generic<K> { inner class Inner<L>(val value: L) }\n\
         fun box(): String {\n\
             typealias Alias<K, L> = Generic<K>.Inner<L>\n\
             val owner = Generic<Int>()\n\
             return owner.Alias<Int, String>(\"OK\").value\n\
         }\n",
        "box",
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32)),
            Some(FirExpr {
                ty,
                kind: FirExprKind::ConstructorCall(_),
                ..
            }) if ty.get().type_args() == [Ty::String, Ty::Int]
        )
    }));
}

#[test]
fn statement_local_alias_publishes_local_class_constructor_signature() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +LocalTypeAliases\n\
         abstract class A { abstract val p: String }\n\
         fun box(): A {\n\
             typealias Alias = String\n\
             class B(override val p: Alias) : A()\n\
             return B(\"OK\")\n\
         }\n",
        "box",
    );
    let call = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        let FirExprKind::ConstructorCall(call) = &expression.kind else {
            return None;
        };
        matches!(expression.ty.get(), Ty::Obj(_, _)).then_some(call)
    });
    let call = call.expect("checked local-class constructor call");
    let FirConstructorTarget::Module(target) = call.target else {
        panic!("local source constructor must retain a module identity")
    };
    let declaration = index
        .callable(target)
        .expect("local constructor callable")
        .declaration;
    assert_eq!(
        index
            .signature(declaration)
            .expect("local constructor signature")
            .parameters[0]
            .get(),
        Ty::String,
    );
}

#[test]
fn local_generic_superclass_uses_its_own_type_parameter_in_constructor_reference() {
    let (body, _) = checked_function_body(
        "open class Base<T>(val value: T)\n\
         fun <T, R> apply(value: T, factory: (T) -> R): R = factory(value)\n\
         fun <T> make(value: T): Base<T> {\n\
             class Local<U>(item: U) : Base<U>(item)\n\
             return apply(value, ::Local)\n\
         }\n",
        "make",
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32)),
            Some(FirExpr {
                ty,
                kind: FirExprKind::CallableReference { .. },
                ..
            }) if matches!(ty.get(), Ty::Fun(signature) if signature.params.len() == 1)
        )
    }));
}

#[test]
fn local_class_member_reads_capture_field_by_checker_selected_ordinal() {
    let (body, _) = checked_function_body(
        "fun box(): String {\n\
             val prefix = \"O\"\n\
             class Local { fun read(): String = prefix + \"K\" }\n\
             return Local().read()\n\
         }\n",
        "read",
    );
    assert!((0..body.expression_count()).any(|raw| {
        matches!(
            body.expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::ClassStorageRead { field: 0, .. })
        )
    }));
}

#[test]
fn local_class_member_write_uses_shared_capture_storage() {
    let source = "fun outer(): String {\n\
                      var value = \"\"\n\
                      class Local {\n\
                          fun mutate(next: String): String { value = next; return value }\n\
                      }\n\
                      return Local().mutate(\"OK\")\n\
                  }\n";
    let (outer, _) = checked_function_body(source, "outer");
    let FirExprKind::Block { statements, .. } = &outer
        .expr(root_expression(&outer))
        .expect("outer block")
        .kind
    else {
        panic!("outer function must retain its checked block")
    };
    let captures = statements
        .iter()
        .filter_map(|statement| outer.statement(*statement))
        .find_map(|statement| match &statement.kind {
            FirStatementKind::LocalDeclaration { captures, .. } => Some(captures.as_ref()),
            _ => None,
        })
        .expect("local class declaration");
    assert!(captures
        .iter()
        .any(|capture| capture.name.as_ref() == "value" && capture.shared_cell));
}

#[test]
fn class_capture_write_retains_nullable_cell_type_over_non_null_rhs() {
    let (_, bodies) = checked_streamed_bodies(
        "class Result<T>\n\
         fun publish(result: Result<String>): Result<String>? {\n\
             var outcome: Result<String>? = null\n\
             val sink = object {\n\
                 fun accept(value: Result<String>) { outcome = value }\n\
             }\n\
             sink.accept(result)\n\
             return outcome\n\
         }\n",
    );
    let writes = bodies
        .iter()
        .flat_map(|body| {
            (0..body.expression_count()).filter_map(move |raw| {
                let expression = body.expr(FirExprId::from_raw(raw as u32))?;
                let FirExprKind::ClassStorageSharedWrite { element, value, .. } = &expression.kind
                else {
                    return None;
                };
                Some((element.get(), body.expr(*value)?.ty.get()))
            })
        })
        .collect::<Vec<_>>();

    let result = Ty::obj_args("Result", &[Ty::String]);
    assert_eq!(writes, [(Ty::nullable(result), result)]);
}

#[test]
fn class_capture_write_lowers_its_declared_cell_type() {
    let mut index = ResolvedModuleIndex::default();
    let classifier = crate::types::type_name("test/CaptureOwner");
    let declaration = index.intern_checked_local_declaration(
        crate::fir::DeclarationAnchor {
            source: SourceFileId::from_raw(0),
            range: crate::diag::Span::new(0, 1),
            owner: None,
            kind: DeclarationKind::Classifier,
            sibling: 0,
        },
        crate::fir::ResolvedDeclarationHeader {
            kind: DeclarationKind::Classifier,
            owner: None,
            name: None,
            visibility: crate::types::Visibility::Private,
            flags: crate::fir::DeclarationFlags::default()
                .with(crate::fir::DeclarationFlags::LOCAL_CLASS, true),
            initialization_order: None,
        },
        "CaptureOwner",
    );
    index
        .publish_classifier_header(declaration, classifier, None, [], [], [], [])
        .unwrap();

    let result = Ty::obj_args("kotlin/Result", &[Ty::String]);
    let nullable_result = Ty::nullable(result);
    let mut ir = crate::ir::IrFile::default();
    let mut owner = crate::ir::IrClass::synthetic(classifier);
    owner.fields.push(crate::ir::IrField::new(
        "outcome".to_string(),
        nullable_result,
    ));
    let owner = ir.add_class(owner);
    ir.checked_classifier_classes.insert(declaration, owner);
    ir.shared_class_capture_fields
        .insert((owner, 0), nullable_result);

    let origin = OriginId::from_raw(0);
    let mut body = FirBody::new(BodyOwnerId::from_raw(declaration.raw()));
    let parameter = body.allocate_local_value();
    body.add_parameter(FirValueParameter {
        origin,
        value: parameter,
        ty: ResolvedTy::new(result).unwrap(),
    });
    let read = body.add_expr(FirExpr {
        origin,
        ty: ResolvedTy::new(result).unwrap(),
        kind: FirExprKind::ValueRead(parameter),
    });
    let write = body.add_expr(FirExpr {
        origin,
        ty: ResolvedTy::new(Ty::Unit).unwrap(),
        kind: FirExprKind::ClassStorageSharedWrite {
            owner: declaration,
            enclosing_depth: 0,
            field: 0,
            element: ResolvedTy::new(nullable_result).unwrap(),
            value: read,
            conversion: None,
        },
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(write),
    });
    body.push_root(statement);

    let lowered = crate::fir_lower::lower_body(body, &index, &mut ir).unwrap();
    let [root] = lowered.roots.as_ref() else {
        panic!("one shared-cell write root expected")
    };
    let crate::ir::IrExpr::RefSet { elem, value, .. } = ir.expr(*root) else {
        panic!("checked class-capture write must lower to a shared-cell write")
    };
    assert_eq!(*elem, nullable_result);
    assert!(matches!(ir.expr(*value), crate::ir::IrExpr::GetValue(1)));
}

#[test]
fn local_class_inferred_properties_see_plain_primary_constructor_parameters() {
    let (body, _) = checked_function_body(
        "fun box(): String {\n\
             var current = 0\n\
             class Node(level: Int) {\n\
                 val left = if (level > 0) Node(level - 1) else null\n\
                 val index = (left?.index ?: current) + 1\n\
             }\n\
             return if (Node(5).index == 6) \"OK\" else \"Fail\"\n\
         }\n",
        "box",
    );
    assert_eq!(
        body.expr(root_expression(&body))
            .expect("checked outer body")
            .ty
            .get(),
        Ty::Nothing
    );
}

#[test]
fn local_classifier_publishes_captured_type_argument_layout_before_construction() {
    let (body, index) = checked_function_body(
        "fun <T> make(value: T): T {\n\
             class Local(val item: T)\n\
             return Local(value).item\n\
         }\n",
        "make",
    );
    let call = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        match &expression.kind {
            FirExprKind::ConstructorCall(call) => Some(call),
            _ => None,
        }
    });
    let call = call.expect("local generic constructor call");
    assert_eq!(call.substitutions.len(), 1);
    let parameter = match call.substitutions[0].parameter {
        FirTypeParameterRef::Module(parameter) => parameter,
        FirTypeParameterRef::External { .. } => {
            panic!("local classifier parameters must not become dependency identities")
        }
    };
    assert!(index.type_parameter_header(parameter).is_some());
}

#[test]
fn local_class_carries_a_value_used_by_an_anonymous_super_argument() {
    let source = "interface Callback { fun invoke(): String }\n\
                  open class Base(val callback: Callback)\n\
                  fun box(): String {\n\
                      val ok = \"OK\"\n\
                      class Local : Base(object : Callback {\n\
                          override fun invoke() = ok\n\
                      })\n\
                      return Local().callback.invoke()\n\
                  }\n";
    let (body, _) = checked_function_body(source, "box");
    let FirExprKind::Block { statements, .. } = &body
        .expr(root_expression(&body))
        .expect("function block")
        .kind
    else {
        panic!("box must have a checked block body")
    };
    let captures = statements
        .iter()
        .filter_map(|statement| body.statement(*statement))
        .find_map(|statement| match &statement.kind {
            FirStatementKind::LocalDeclaration { captures, .. } => Some(captures.as_ref()),
            _ => None,
        })
        .expect("local class declaration");
    assert!(captures
        .iter()
        .any(|capture| capture.name.as_ref() == "ok" && capture.ty.get() == Ty::String));

    let (invoke, _) = checked_function_body(source, "invoke");
    assert!((0..invoke.expression_count()).any(|raw| {
        matches!(
            invoke
                .expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::ClassStorageRead { field: 0, .. })
        )
    }));
}

#[test]
fn local_class_captures_an_enclosing_extension_receiver_by_checked_coordinate() {
    let source = "class Acc { fun add(value: String): Boolean = true }\n\
                  fun Acc.box(): Boolean {\n\
                      class Local { fun action(): Boolean = add(\"OK\") }\n\
                      return Local().action()\n\
                  }\n";
    let (outer, _) = checked_function_body(source, "box");
    let FirExprKind::Block { statements, .. } = &outer
        .expr(root_expression(&outer))
        .expect("extension body block")
        .kind
    else {
        panic!("extension function must retain its checked block")
    };
    let captures = statements
        .iter()
        .filter_map(|statement| outer.statement(*statement))
        .find_map(|statement| match &statement.kind {
            FirStatementKind::LocalDeclaration { captures, .. } => Some(captures.as_ref()),
            _ => None,
        })
        .expect("local class declaration");
    assert!(captures.iter().any(|capture| {
        capture
            .ty
            .get()
            .obj_internal()
            .is_some_and(|ty| ty.matches("Acc"))
            && matches!(
                &capture.source,
                FirLocalClassCaptureSource::ImplicitReceiver {
                    current: true,
                    depth: 0
                }
            )
    }));

    let (member, _) = checked_function_body(source, "action");
    assert!((0..member.expression_count()).any(|raw| {
        matches!(
            member
                .expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind),
            Some(FirExprKind::ClassStorageRead { .. })
        )
    }));
}

#[test]
fn local_class_captures_both_extension_and_outer_dispatch_receivers() {
    let source = "class Outer(val suffix: String) {\n\
                      fun Receiver.call(): String {\n\
                          class Local {\n\
                              fun read(): String = this@call.value + this@Outer.suffix\n\
                          }\n\
                          return Local().read()\n\
                      }\n\
                  }\n\
                  class Receiver(val value: String)\n";
    let (outer, _) = checked_function_body(source, "call");
    let FirExprKind::Block { statements, .. } = &outer
        .expr(root_expression(&outer))
        .expect("extension body block")
        .kind
    else {
        panic!("extension function must retain its checked block")
    };
    let captures = statements
        .iter()
        .filter_map(|statement| outer.statement(*statement))
        .find_map(|statement| match &statement.kind {
            FirStatementKind::LocalDeclaration { captures, .. } => Some(captures.as_ref()),
            _ => None,
        })
        .expect("local class declaration");
    assert_eq!(captures.len(), 2, "{captures:?}");
    assert!(captures.iter().any(|capture| {
        capture.ty.get() == Ty::obj("Receiver")
            && matches!(
                capture.source,
                FirLocalClassCaptureSource::ImplicitReceiver {
                    current: true,
                    depth: 0
                }
            )
    }));
    assert!(captures
        .iter()
        .any(|capture| capture.ty.get() == Ty::obj("Outer")));
}

#[test]
fn local_class_reads_receiver_lambda_property_through_its_captured_receiver() {
    let (body, _) = checked_function_body(
        "class Environment(val value: String, val action: Environment.() -> Unit)\n\
         fun use() {\n\
             Environment(\"OK\") { class Local { val captured = value } }\n\
         }\n",
        "use",
    );
    let lambda = (0..body.expression_count())
        .find_map(
            |raw| match &body.expr(FirExprId::from_raw(raw as u32))?.kind {
                FirExprKind::Lambda { body, .. } => Some(body),
                _ => None,
            },
        )
        .expect("receiver lambda body");
    let captures = (0..lambda.statement_count())
        .find_map(
            |raw| match &lambda.statement(FirStatementId::from_raw(raw as u32))?.kind {
                FirStatementKind::LocalDeclaration { captures, .. } => Some(captures.as_ref()),
                _ => None,
            },
        )
        .expect("local class declaration");
    let [receiver] = captures else {
        panic!("receiver property must not become a duplicate lexical capture: {captures:?}")
    };
    assert_eq!(receiver.name.as_ref(), "this$receiver");
    assert!(matches!(
        &receiver.source,
        FirLocalClassCaptureSource::ImplicitReceiver {
            current: true,
            depth: 0
        }
    ));
}

#[test]
fn anonymous_object_generic_member_result_is_published_before_later_local_use() {
    let (body, _) = checked_function_body(
        "fun <T> test(): String {\n\
             val value = object { fun <S> get() = \"OK\" }\n\
             return value.get<Any>()\n\
         }\n",
        "test",
    );
    let call = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        match &expression.kind {
            FirExprKind::Call(call) if expression.ty.get() == Ty::String => Some(call),
            _ => None,
        }
    });
    let call = call.expect("checked anonymous-object member call");
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), Ty::obj("kotlin/Any"));
}

#[test]
fn anonymous_object_superclass_uses_the_statement_local_alias_scope() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +LocalTypeAliases\n\
         fun box(): String {\n\
             open class Local { fun test(): String = \"OK\" }\n\
             typealias Alias = Local\n\
             val value = object : Alias() {}\n\
             return value.test()\n\
         }\n",
        "box",
    );
    let call = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        let FirExprKind::Call(call) = &expression.kind else {
            return None;
        };
        let target = call.target.module()?;
        (index.callable_name(target) == Some("test")).then_some(call)
    });
    assert!(
        call.is_some(),
        "inherited call must retain its checked target"
    );
}

#[test]
fn local_class_header_sees_nested_classifier_inherited_by_lexical_owner() {
    let (body, index) = checked_function_body(
        "open class Outer {\n\
             open inner class A {\n\
                 open fun foo(x: String, y: String? = null): String = x + (y ?: \"K\")\n\
             }\n\
         }\n\
         fun box(): String {\n\
             val derived = object : Outer() {\n\
                 inner class Local : A() {\n\
                     override fun foo(x: String, y: String?) = super.foo(x, y)\n\
                 }\n\
             }\n\
             return derived.Local().foo(\"O\")\n\
         }\n",
        "box",
    );
    let call = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        let FirExprKind::Call(call) = &expression.kind else {
            return None;
        };
        let target = call.target.module()?;
        (index.callable_name(target) == Some("foo")).then_some(call)
    });
    let call = call.expect("inherited override call");
    assert!(call
        .arguments
        .iter()
        .any(|argument| matches!(argument, FirCallArgument::Default { parameter: 1, .. })));
}

#[test]
fn body_local_super_call_uses_the_checked_local_superclass_rung() {
    production_frontend_ok(
        "fun box(): String {\n\
             val captured = \"O\"\n\
             open class Base { open fun value() = captured }\n\
             class Derived : Base() { override fun value() = super.value() + \"K\" }\n\
             return Derived().value()\n\
         }\n",
    );
}

#[test]
fn inherited_body_local_call_keeps_the_subclass_dispatch_receiver_for_protected_access() {
    production_frontend_ok(
        "fun box(): String {\n\
             abstract class Base { protected open fun value() = \"OK\" }\n\
             class Derived : Base() { fun read() = value() }\n\
             return Derived().read()\n\
         }\n",
    );
}
