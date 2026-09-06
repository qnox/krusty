use crate::ast::{Decl, Expr};
use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;
use crate::types::Ty;

use super::*;

#[derive(Default)]
struct RecordingSink(Vec<BodyOwnerId>);

impl CheckedBodySink for RecordingSink {
    fn accept_finalized(&mut self, owner: BodyOwnerId, body: FirBody) {
        assert_eq!(body.owner(), owner);
        self.0.push(owner);
    }
}

fn checked_analysis(source: &str) -> crate::frontend::SourceSetAnalysis {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Body")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    analysis
}

fn stable_declaration_at(
    analysis: &crate::frontend::SourceSetAnalysis,
    span: crate::diag::Span,
    kind: DeclarationKind,
) -> DeclarationId {
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let index = streamed.module.index();
    let source = SourceFileId::from_raw(0);
    let active = ActiveSourceDeclarations::bind_complete_source(&analysis.files[0], source, index)
        .expect("the retained test AST must bind to the stable declaration stream");
    (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            index
                .declaration_anchor(*declaration)
                .is_some_and(|anchor| anchor.kind == kind)
                && active.span(&analysis.files[0], *declaration) == Some(span)
        })
        .max_by_key(|declaration| index.declaration_header(*declaration).is_some())
        .expect("stable declaration at active span")
}

#[test]
fn checked_structural_body_owns_no_ast_and_preserves_local_identity() {
    let analysis = checked_analysis(
        r#"
val body = {
    var value = "a"
    value = "b"
    "$value"
}
"#,
    );
    let file = &analysis.files[0];
    let info = analysis.types[0].as_ref().expect("file must be checked");
    let lambda = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Property(property) => property.init,
            Decl::Fun(_) | Decl::Class(_) => None,
        });
    let block = match file.expr(lambda.expect("property initializer")) {
        Expr::Lambda { body, .. } => *body,
        expression => panic!("expected lambda, found {expression:?}"),
    };
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(7),
        block,
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("structural forms must build checked FIR");

    drop(analysis);

    let root = body.statement(body.roots()[0]).expect("root statement");
    let FirStatementKind::Expression(root) = root.kind else {
        panic!("body root must be an expression")
    };
    let FirExprKind::Block { statements, result } = &body.expr(root).expect("root expression").kind
    else {
        panic!("lambda body must remain a checked FIR block")
    };
    assert_eq!(statements.len(), 2);
    let FirStatementKind::Local { target, .. } =
        body.statement(statements[0]).expect("local statement").kind
    else {
        panic!("first statement must declare the local")
    };
    let FirStatementKind::Expression(write) = body
        .statement(statements[1])
        .expect("assignment statement")
        .kind
    else {
        panic!("second statement must write the local")
    };
    assert!(matches!(
        body.expr(write).map(|expression| &expression.kind),
        Some(FirExprKind::ValueWrite { target: written, .. }) if *written == target
    ));
    let result = result.expect("block result");
    assert!(matches!(
        body.expr(result).map(|expression| &expression.kind),
        Some(FirExprKind::StringTemplate(parts)) if parts.len() == 1
    ));
    assert!(!origins.is_empty());
}

#[test]
fn lambda_body_form_builds_owned_checked_fir() {
    let analysis = checked_analysis("val result = { 1 }\n");
    let file = &analysis.files[0];
    let info = analysis.types[0].as_ref().expect("file must be checked");
    let call = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Property(property) if property.name == "result" => property.init,
            Decl::Property(_) | Decl::Fun(_) | Decl::Class(_) => None,
        });
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(9),
        call.expect("call initializer"),
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("lambda body must become checked FIR");
    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("lambda initializer must be an expression root")
    };
    let FirExprKind::Lambda { body: lambda, .. } = &body.expr(root).unwrap().kind else {
        panic!("lambda syntax must become an owned FIR lambda")
    };
    assert_eq!(lambda.roots().len(), 1);
}

#[test]
fn checked_builtin_operators_are_explicit_fir_decisions() {
    let analysis = checked_analysis("val result = if (1 + 2 * 3 < 8) 4 else 9\n");
    let file = &analysis.files[0];
    let info = analysis.types[0].as_ref().expect("file must be checked");
    let root = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Property(property) => property.init,
            Decl::Fun(_) | Decl::Class(_) => None,
        });
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(11),
        root.expect("property initializer"),
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("checker-selected builtin operators must build FIR");

    let FirStatementKind::Expression(root) = body
        .statement(body.roots()[0])
        .expect("root statement")
        .kind
    else {
        panic!("body root must be an expression")
    };
    let FirExprKind::Conditional { condition, .. } = body.expr(root).expect("root expression").kind
    else {
        panic!("root must be conditional FIR")
    };
    assert!(matches!(
        body.expr(condition).map(|expression| &expression.kind),
        Some(FirExprKind::Binary {
            operation: FirBinaryOperation::Less,
            ..
        })
    ));
    let FirExprKind::Binary { lhs, .. } = body.expr(condition).expect("comparison").kind else {
        panic!("condition must be binary FIR")
    };
    assert!(matches!(
        body.expr(lhs).map(|expression| &expression.kind),
        Some(FirExprKind::Binary {
            operation: FirBinaryOperation::Add,
            ..
        })
    ));
}

#[test]
fn checked_const_expression_is_published_as_one_fir_constant() {
    let analysis = checked_analysis("const val four = 2 + 2\n");
    let file = &analysis.files[0];
    let info = analysis.types[0].as_ref().expect("file must be checked");
    let root = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Property(property) => property.init,
            Decl::Fun(_) | Decl::Class(_) => None,
        })
        .expect("const initializer");
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(12),
        root,
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("checked const expression must build FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("const initializer must be an expression root")
    };
    assert!(matches!(
        body.expr(root).map(|expression| &expression.kind),
        Some(FirExprKind::Constant(FirConstant::Int(4)))
    ));
}

#[test]
fn pass_one_retains_inline_fir_before_ordinary_body_streaming() {
    let mut analysis = checked_analysis("inline fun kept() = 1 + 2\nfun streamed() = 3 + 4\n");
    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary_bodies =
        streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();

    assert_eq!(inline_bodies.len(), 1);
    assert!(sink.0.is_empty());

    for work in ordinary_bodies {
        let source = index
            .declaration_anchor(work.declaration)
            .expect("scheduled declaration")
            .source;
        check_and_dispatch_body(
            &analysis.files[0],
            analysis.types[0].as_ref().expect("checked file"),
            source,
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("ordinary body must stream");
    }
    assert_eq!(sink.0.len(), 1);
    assert_eq!(inline_bodies.len(), 1);
}

#[test]
fn cross_file_source_call_reaches_fir_as_a_stable_callable_identity() {
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[
            SourceInput::kotlin("fun <T> identity(value: T) = value\n").with_file_stem("Identity"),
            SourceInput::kotlin("fun caller() = identity(\"OK\")\n").with_file_stem("Caller"),
        ],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let answer_span = match analysis.files[0].decl(analysis.files[0].decls[0]) {
        Decl::Fun(function) => function.span,
        Decl::Property(_) | Decl::Class(_) => panic!("answer must be a function"),
    };
    let answer = stable_declaration_at(&analysis, answer_span, DeclarationKind::Function);
    let expected_target = streamed
        .module
        .index()
        .callable_for_declaration(answer)
        .expect("identity callable")
        .id;
    let caller = match analysis.files[1].decl(analysis.files[1].decls[0]) {
        Decl::Fun(function) => match function.body {
            crate::ast::FunBody::Expr(expression) => expression,
            crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => {
                panic!("caller must have an expression body")
            }
        },
        Decl::Property(_) | Decl::Class(_) => panic!("caller must be a function"),
    };
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        &analysis.files[1],
        analysis.types[1].as_ref().expect("checked caller"),
        SourceFileId::from_raw(1),
        BodyOwnerId::from_raw(17),
        caller,
        streamed.module.index(),
        &mut origins,
    )
    .expect("source call must build checked FIR");

    let FirStatementKind::Expression(root) = body
        .statement(body.roots()[0])
        .expect("root statement")
        .kind
    else {
        panic!("body root must be expression FIR")
    };
    let Some(FirExprKind::Call(call)) = body.expr(root).map(|expression| &expression.kind) else {
        panic!("root must be checked call FIR")
    };
    assert_eq!(call.target, expected_target.into());
    assert_eq!(call.arguments.len(), 1);
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), crate::types::Ty::String);
}

#[test]
fn checked_source_call_embeds_named_mapping_and_default_slots() {
    let analysis = checked_analysis(
        "fun target(first: String = \"default\", second: Int) = first\n\
         fun caller() = target(second = 7)\n",
    );
    let file = &analysis.files[0];
    let caller = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == "caller" => match function.body {
                crate::ast::FunBody::Expr(expression) => Some(expression),
                crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => None,
            },
            Decl::Fun(_) | Decl::Property(_) | Decl::Class(_) => None,
        });
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        file,
        analysis.types[0].as_ref().expect("checked file"),
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(19),
        caller.expect("caller expression"),
        streamed.module.index(),
        &mut origins,
    )
    .expect("mapped source call must build checked FIR");

    let FirStatementKind::Expression(root) = body
        .statement(body.roots()[0])
        .expect("root statement")
        .kind
    else {
        panic!("body root must be expression FIR")
    };
    let Some(FirExprKind::Call(call)) = body.expr(root).map(|expression| &expression.kind) else {
        panic!("root must be checked call FIR")
    };
    assert!(matches!(
        call.arguments.as_ref(),
        [
            FirCallArgument::Expression { parameter: 1, .. },
            FirCallArgument::Default { parameter: 0, .. }
        ]
    ));
}

#[test]
fn block_body_parameters_and_returns_use_body_local_stable_targets() {
    let analysis = checked_analysis("fun echo(value: String): String { return value }\n");
    let file = &analysis.files[0];
    let function = match file.decl(file.decls[0]) {
        Decl::Fun(function) => function,
        Decl::Property(_) | Decl::Class(_) => panic!("echo must be a function"),
    };
    let root = match function.body {
        crate::ast::FunBody::Block(root) => root,
        crate::ast::FunBody::Expr(_) | crate::ast::FunBody::None => {
            panic!("echo must have a block body")
        }
    };
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let declaration = stable_declaration_at(&analysis, function.span, DeclarationKind::Function);
    let parameter_ty = streamed
        .module
        .index()
        .signature(declaration)
        .unwrap()
        .parameters[0];
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        analysis.types[0].as_ref().expect("checked file"),
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(declaration.raw()),
        root,
        &[CheckedBodyParameter {
            name: &function.params[0].name,
            ty: parameter_ty,
            span: function.params[0].ty.span,
        }],
        streamed.module.index(),
        &mut origins,
    )
    .expect("block body must build checked FIR");

    let [parameter] = body.parameters() else {
        panic!("one FIR value parameter expected")
    };
    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root statement must contain the block")
    };
    let FirExprKind::Block { statements, .. } = &body.expr(root).unwrap().kind else {
        panic!("root expression must be a block")
    };
    let FirStatementKind::Expression(jump) = body.statement(statements[0]).unwrap().kind else {
        panic!("return statement must contain a jump")
    };
    let FirExprKind::Jump {
        kind: FirJumpKind::Return { target_depth: 0 },
        target,
        value: Some(value),
    } = body.expr(jump).unwrap().kind
    else {
        panic!("checked return must carry its target and value")
    };
    assert_eq!(
        body.control_target(target).unwrap().kind,
        FirControlTargetKind::Body(BodyOwnerId::from_raw(declaration.raw()))
    );
    assert!(matches!(
        body.expr(value).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(local)) if *local == parameter.value
    ));
}

#[test]
fn selected_source_member_call_keeps_stable_target_receiver_and_default_mapping() {
    let analysis = checked_analysis(
        "class Box { fun take(value: Int = 1): Int = value }\n\
         fun caller(box: Box) = box.take()\n",
    );
    let file = &analysis.files[0];
    let (member_span, caller) = file.decls.iter().fold(
        (None, None),
        |(member_span, caller), declaration| match file.decl(*declaration) {
            Decl::Class(class) => (
                class
                    .methods
                    .iter()
                    .find(|method| method.name == "take")
                    .map(|method| method.span)
                    .or(member_span),
                caller,
            ),
            Decl::Fun(function) if function.name == "caller" => (member_span, Some(function)),
            Decl::Fun(_) | Decl::Property(_) => (member_span, caller),
        },
    );
    let member_span = member_span.expect("take member span");
    let caller = caller.expect("caller declaration");
    let root = match caller.body {
        crate::ast::FunBody::Expr(root) => root,
        crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => {
            panic!("caller must have an expression body")
        }
    };
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let declaration = stable_declaration_at(&analysis, member_span, DeclarationKind::Function);
    let expected_target = streamed
        .module
        .index()
        .callable_for_declaration(declaration)
        .expect("stable member callable")
        .id;
    let parameter_ty = analysis.types[0]
        .as_ref()
        .expect("checked file")
        .resolved_type(&caller.params[0].ty)
        .and_then(|ty| ResolvedTy::new(ty).ok())
        .expect("resolved Box parameter type");
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        analysis.types[0].as_ref().expect("checked file"),
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(23),
        root,
        &[CheckedBodyParameter {
            name: &caller.params[0].name,
            ty: parameter_ty,
            span: caller.params[0].ty.span,
        }],
        streamed.module.index(),
        &mut origins,
    )
    .expect("selected source member call must build checked FIR");

    let [parameter] = body.parameters() else {
        panic!("one receiver parameter expected")
    };
    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root statement must contain the call")
    };
    let FirExprKind::Call(call) = &body.expr(root).unwrap().kind else {
        panic!("root expression must be a checked call")
    };
    assert_eq!(call.target, expected_target.into());
    assert!(matches!(
        call.dispatch_receiver
            .and_then(|receiver| body.expr(receiver.value))
            .map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(value)) if *value == parameter.value
    ));
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Default { parameter: 0, .. }]
    ));
}

#[test]
fn selected_source_operator_is_a_stable_fir_call_not_a_spelling_based_binary() {
    let analysis = checked_analysis(
        "class Box { operator fun plus(other: Box): Box = other }\n\
         fun caller(left: Box, right: Box) = left + right\n",
    );
    let file = &analysis.files[0];
    let class = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Class(class) if class.name == "Box" => Some(class),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("Box declaration");
    let plus = class
        .methods
        .iter()
        .find(|method| method.name == "plus")
        .expect("plus declaration");
    let caller = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == "caller" => Some(function),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("caller declaration");
    let root = match caller.body {
        crate::ast::FunBody::Expr(root) => root,
        crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => {
            panic!("caller must have an expression body")
        }
    };
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let declaration = stable_declaration_at(&analysis, plus.span, DeclarationKind::Function);
    let expected_target = streamed
        .module
        .index()
        .callable_for_declaration(declaration)
        .expect("stable plus callable")
        .id;
    let info = analysis.types[0].as_ref().expect("checked file");
    let parameters = caller
        .params
        .iter()
        .map(|parameter| CheckedBodyParameter {
            name: &parameter.name,
            ty: ResolvedTy::new(
                info.resolved_type(&parameter.ty)
                    .expect("resolved Box parameter type"),
            )
            .expect("publishable Box parameter type"),
            span: parameter.ty.span,
        })
        .collect::<Vec<_>>();
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(29),
        root,
        &parameters,
        streamed.module.index(),
        &mut origins,
    )
    .expect("selected source operator must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root statement must contain the operator call")
    };
    let FirExprKind::Call(call) = &body.expr(root).unwrap().kind else {
        panic!("custom operator must become an exact FIR call")
    };
    assert_eq!(call.target, expected_target.into());
    assert!(call.dispatch_receiver.is_some());
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Expression { parameter: 0, .. }]
    ));
}

#[test]
fn labeled_loop_jumps_bind_to_stable_fir_control_targets() {
    let analysis = checked_analysis(
        "fun run(): Int {\n\
             outer@ while (true) {\n\
                 do { break@outer } while (false)\n\
             }\n\
             return 1\n\
         }\n",
    );
    let file = &analysis.files[0];
    let function = match file.decl(file.decls[0]) {
        Decl::Fun(function) => function,
        Decl::Class(_) | Decl::Property(_) => panic!("run must be a function"),
    };
    let root = match function.body {
        crate::ast::FunBody::Block(root) => root,
        crate::ast::FunBody::Expr(_) | crate::ast::FunBody::None => {
            panic!("run must have a block body")
        }
    };
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        file,
        analysis.types[0].as_ref().expect("checked file"),
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(31),
        root,
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("labeled loops must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain the function block")
    };
    let FirExprKind::Block { statements, .. } = &body.expr(root).unwrap().kind else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Loop {
        target: outer_target,
        body: outer_body,
        ..
    } = body.statement(statements[0]).unwrap().kind
    else {
        panic!("first statement must be the outer loop")
    };
    let FirExprKind::Block {
        statements: outer_statements,
        ..
    } = &body.expr(outer_body).unwrap().kind
    else {
        panic!("outer body must be a block")
    };
    let FirStatementKind::Loop {
        body: inner_body, ..
    } = body.statement(outer_statements[0]).unwrap().kind
    else {
        panic!("outer body must contain the do-while loop")
    };
    let FirExprKind::Block {
        statements: inner_statements,
        ..
    } = &body.expr(inner_body).unwrap().kind
    else {
        panic!("inner body must be a block")
    };
    let FirStatementKind::Expression(jump) = body.statement(inner_statements[0]).unwrap().kind
    else {
        panic!("inner body must contain the break jump")
    };
    assert!(matches!(
        body.expr(jump).map(|expression| &expression.kind),
        Some(FirExprKind::Jump {
            kind: FirJumpKind::Break { target_depth: 0 },
            target,
            value: None,
        }) if *target == outer_target
    ));
}

#[test]
fn range_loop_header_owns_the_same_local_identity_read_by_its_body() {
    let analysis = checked_analysis("fun run() { for (index in 1..3) { index } }\n");
    let file = &analysis.files[0];
    let function = match file.decl(file.decls[0]) {
        Decl::Fun(function) => function,
        Decl::Class(_) | Decl::Property(_) => panic!("run must be a function"),
    };
    let root = match function.body {
        crate::ast::FunBody::Block(root) => root,
        crate::ast::FunBody::Expr(_) | crate::ast::FunBody::None => {
            panic!("run must have a block body")
        }
    };
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        file,
        analysis.types[0].as_ref().expect("checked file"),
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(37),
        root,
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("range loop must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain the function block")
    };
    let FirExprKind::Block { statements, .. } = &body.expr(root).unwrap().kind else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Loop {
        header:
            FirLoopHeader::Range {
                variable,
                counter,
                operation: FirRangeOperation::Through,
                ..
            },
        body: loop_body,
        ..
    } = &body.statement(statements[0]).unwrap().kind
    else {
        panic!("first statement must be an explicit range loop")
    };
    assert_eq!(counter.ty(), crate::types::Ty::Int);
    let FirExprKind::Block { result, .. } = &body.expr(*loop_body).unwrap().kind else {
        panic!("loop body must be a block")
    };
    assert!(matches!(
        result.and_then(|result| body.expr(result)).map(|expression| &expression.kind),
        Some(FirExprKind::ValueRead(value)) if value == variable
    ));
}

#[test]
fn builtin_increment_statements_and_postfix_values_are_explicit_fir_updates() {
    let analysis = checked_analysis(
        "fun mutate(): Int {\n\
             var value = 1\n\
             ++value\n\
             return value++\n\
         }\n",
    );
    let file = &analysis.files[0];
    let function = match file.decl(file.decls[0]) {
        Decl::Fun(function) => function,
        Decl::Class(_) | Decl::Property(_) => panic!("mutate must be a function"),
    };
    let root = match function.body {
        crate::ast::FunBody::Block(root) => root,
        crate::ast::FunBody::Expr(_) | crate::ast::FunBody::None => {
            panic!("mutate must have a block body")
        }
    };
    let mut origins = OriginStore::default();
    let body = check_expression_body(
        file,
        analysis.types[0].as_ref().expect("checked file"),
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(41),
        root,
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("builtin increment forms must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain the function block")
    };
    let FirExprKind::Block { statements, .. } = &body.expr(root).unwrap().kind else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(prefix_write) = body.statement(statements[1]).unwrap().kind
    else {
        panic!("prefix statement must be a write")
    };
    let FirExprKind::ValueWrite {
        value: prefix_value,
        ..
    } = body.expr(prefix_write).unwrap().kind
    else {
        panic!("prefix statement must write its updated value")
    };
    assert!(matches!(
        body.expr(prefix_value).map(|expression| &expression.kind),
        Some(FirExprKind::Unary {
            operation: FirUnaryOperation::Increment,
            ..
        })
    ));
    let FirStatementKind::Expression(return_jump) = body.statement(statements[2]).unwrap().kind
    else {
        panic!("return must contain a jump")
    };
    let FirExprKind::Jump {
        value: Some(postfix),
        ..
    } = body.expr(return_jump).unwrap().kind
    else {
        panic!("return must carry the postfix expression")
    };
    let FirExprKind::Block {
        statements: postfix_statements,
        result: Some(_),
    } = &body.expr(postfix).unwrap().kind
    else {
        panic!("postfix increment must preserve its old value in a FIR block")
    };
    assert_eq!(postfix_statements.len(), 2);
}

#[test]
fn selected_compare_to_keeps_both_callable_identity_and_comparison_semantics() {
    let analysis = checked_analysis(
        "class Box { operator fun compareTo(other: Box): Int = 0 }\n\
         fun caller(left: Box, right: Box) = left < right\n",
    );
    let file = &analysis.files[0];
    let class = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Class(class) if class.name == "Box" => Some(class),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("Box declaration");
    let compare_to = class
        .methods
        .iter()
        .find(|method| method.name == "compareTo")
        .expect("compareTo declaration");
    let caller = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == "caller" => Some(function),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("caller declaration");
    let root = match caller.body {
        crate::ast::FunBody::Expr(root) => root,
        crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => {
            panic!("caller must have an expression body")
        }
    };
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let declaration = stable_declaration_at(&analysis, compare_to.span, DeclarationKind::Function);
    let expected_target = streamed
        .module
        .index()
        .callable_for_declaration(declaration)
        .expect("stable compareTo callable")
        .id;
    let info = analysis.types[0].as_ref().expect("checked file");
    let parameters = caller
        .params
        .iter()
        .map(|parameter| CheckedBodyParameter {
            name: &parameter.name,
            ty: ResolvedTy::new(info.resolved_type(&parameter.ty).unwrap()).unwrap(),
            span: parameter.ty.span,
        })
        .collect::<Vec<_>>();
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(43),
        root,
        &parameters,
        streamed.module.index(),
        &mut origins,
    )
    .expect("selected comparison must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain comparison FIR")
    };
    let FirExprKind::ComparisonCall {
        operation: FirBinaryOperation::Less,
        call,
    } = &body.expr(root).unwrap().kind
    else {
        panic!("comparison must retain its compareTo result test")
    };
    assert_eq!(call.target, expected_target.into());
    assert_eq!(body.expr(root).unwrap().ty.get(), crate::types::Ty::Boolean);
}

#[test]
fn selected_range_to_operator_is_a_stable_fir_call() {
    let analysis = checked_analysis(
        "class Span\n\
         class Bound { operator fun rangeTo(other: Bound): Span = Span() }\n\
         fun caller(left: Bound, right: Bound) = left..right\n",
    );
    let file = &analysis.files[0];
    let class = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Class(class) if class.name == "Bound" => Some(class),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("Bound declaration");
    let range_to = class
        .methods
        .iter()
        .find(|method| method.name == "rangeTo")
        .expect("rangeTo declaration");
    let caller = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == "caller" => Some(function),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("caller declaration");
    let root = match caller.body {
        crate::ast::FunBody::Expr(root) => root,
        crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => {
            panic!("caller must have an expression body")
        }
    };
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let declaration = stable_declaration_at(&analysis, range_to.span, DeclarationKind::Function);
    let expected_target = streamed
        .module
        .index()
        .callable_for_declaration(declaration)
        .expect("stable rangeTo callable")
        .id;
    let info = analysis.types[0].as_ref().expect("checked file");
    let parameters = caller
        .params
        .iter()
        .map(|parameter| CheckedBodyParameter {
            name: &parameter.name,
            ty: ResolvedTy::new(info.resolved_type(&parameter.ty).unwrap()).unwrap(),
            span: parameter.ty.span,
        })
        .collect::<Vec<_>>();
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(47),
        root,
        &parameters,
        streamed.module.index(),
        &mut origins,
    )
    .expect("selected rangeTo must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain rangeTo FIR")
    };
    let FirExprKind::Call(call) = &body.expr(root).unwrap().kind else {
        panic!("custom rangeTo must be an exact FIR call")
    };
    assert_eq!(call.target, expected_target.into());
}

#[test]
fn indexed_reads_distinguish_builtin_storage_from_selected_get_calls() {
    let analysis = checked_analysis(
        "fun arrayAt(values: IntArray, index: Int) = values[index]\n\
         class Box { operator fun get(index: Int): String = \"OK\" }\n\
         fun boxAt(box: Box, index: Int) = box[index]\n",
    );
    let file = &analysis.files[0];
    let info = analysis.types[0].as_ref().expect("checked file");
    let streamed = analysis.streamed.as_ref().expect("Pass 1 must finalize");
    let function = |name: &str| {
        file.decls
            .iter()
            .find_map(|declaration| match file.decl(*declaration) {
                Decl::Fun(function) if function.name == name => Some(function),
                Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
            })
            .unwrap()
    };
    let check =
        |function: &crate::ast::FunDecl, owner: u32, origins: &mut OriginStore| -> FirBody {
            let root = match function.body {
                crate::ast::FunBody::Expr(root) => root,
                crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => {
                    panic!("test function must have an expression body")
                }
            };
            let parameters = function
                .params
                .iter()
                .map(|parameter| CheckedBodyParameter {
                    name: &parameter.name,
                    ty: ResolvedTy::new(info.resolved_type(&parameter.ty).unwrap()).unwrap(),
                    span: parameter.ty.span,
                })
                .collect::<Vec<_>>();
            check_expression_body_with_parameters(
                file,
                info,
                SourceFileId::from_raw(0),
                BodyOwnerId::from_raw(owner),
                root,
                &parameters,
                streamed.module.index(),
                origins,
            )
            .expect("indexed read must build checked FIR")
        };

    let mut origins = OriginStore::default();
    let array_body = check(function("arrayAt"), 51, &mut origins);
    let FirStatementKind::Expression(array_root) =
        array_body.statement(array_body.roots()[0]).unwrap().kind
    else {
        panic!("arrayAt root must be expression FIR")
    };
    assert!(matches!(
        array_body.expr(array_root).map(|expression| &expression.kind),
        Some(FirExprKind::IndexedRead {
            kind: FirIndexedAccessKind::Array,
            indices,
            ..
        }) if indices.len() == 1
    ));

    let box_body = check(function("boxAt"), 53, &mut origins);
    let FirStatementKind::Expression(box_root) =
        box_body.statement(box_body.roots()[0]).unwrap().kind
    else {
        panic!("boxAt root must be expression FIR")
    };
    let FirExprKind::Call(call) = &box_body.expr(box_root).unwrap().kind else {
        panic!("custom get must be a stable FIR call")
    };
    assert!(call.dispatch_receiver.is_some());
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Expression { parameter: 0, .. }]
    ));
}

#[test]
fn builtin_index_assignment_is_an_explicit_checked_fir_write() {
    let analysis = checked_analysis(
        "fun write(values: IntArray, index: Int, value: Int) { values[index] = value }\n",
    );
    let file = &analysis.files[0];
    let function = match file.decl(file.decls[0]) {
        Decl::Fun(function) => function,
        Decl::Class(_) | Decl::Property(_) => panic!("write must be a function"),
    };
    let root = match function.body {
        crate::ast::FunBody::Block(root) => root,
        crate::ast::FunBody::Expr(_) | crate::ast::FunBody::None => {
            panic!("write must have a block body")
        }
    };
    let info = analysis.types[0].as_ref().expect("checked file");
    let parameters = function
        .params
        .iter()
        .map(|parameter| CheckedBodyParameter {
            name: &parameter.name,
            ty: ResolvedTy::new(info.resolved_type(&parameter.ty).unwrap()).unwrap(),
            span: parameter.ty.span,
        })
        .collect::<Vec<_>>();
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(59),
        root,
        &parameters,
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("array assignment must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain the function block")
    };
    let FirExprKind::Block { statements, .. } = &body.expr(root).unwrap().kind else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(write) = body.statement(statements[0]).unwrap().kind else {
        panic!("body must contain the indexed write")
    };
    assert!(matches!(
        body.expr(write).map(|expression| &expression.kind),
        Some(FirExprKind::IndexedWrite {
            indices,
            ..
        }) if indices.len() == 1
    ));
}

#[test]
fn selected_index_set_operator_is_a_stable_fir_call() {
    let analysis = checked_analysis(
        "class Box { operator fun set(index: Int, value: String) {} }\n\
         fun write(box: Box) { box[1] = \"OK\" }\n",
    );
    let file = &analysis.files[0];
    let function = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == "write" => Some(function),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("write declaration");
    let root = match function.body {
        crate::ast::FunBody::Block(root) => root,
        crate::ast::FunBody::Expr(_) | crate::ast::FunBody::None => {
            panic!("write must have a block body")
        }
    };
    let info = analysis.types[0].as_ref().expect("checked file");
    let parameter = CheckedBodyParameter {
        name: &function.params[0].name,
        ty: ResolvedTy::new(info.resolved_type(&function.params[0].ty).unwrap()).unwrap(),
        span: function.params[0].ty.span,
    };
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(61),
        root,
        &[parameter],
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("selected set operator must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain the function block")
    };
    let FirExprKind::Block { statements, .. } = &body.expr(root).unwrap().kind else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(set) = body.statement(statements[0]).unwrap().kind else {
        panic!("body must contain the set call")
    };
    let FirExprKind::Call(call) = &body.expr(set).unwrap().kind else {
        panic!("custom set must be a stable FIR call")
    };
    assert!(call.dispatch_receiver.is_some());
    assert_eq!(call.arguments.len(), 2);
}

#[test]
fn indexed_assignment_explicitly_discards_a_non_unit_set_result() {
    let analysis = checked_analysis(
        "class Box { operator fun set(index: Int, value: String): String = value }\n\
         fun write(box: Box) { box[1] = \"OK\" }\n",
    );
    let file = &analysis.files[0];
    let function = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == "write" => Some(function),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("write declaration");
    let crate::ast::FunBody::Block(root) = function.body else {
        panic!("write must have a block body")
    };
    let info = analysis.types[0].as_ref().expect("checked file");
    let parameter = CheckedBodyParameter {
        name: &function.params[0].name,
        ty: ResolvedTy::new(info.resolved_type(&function.params[0].ty).unwrap()).unwrap(),
        span: function.params[0].ty.span,
    };
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(62),
        root,
        &[parameter],
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("selected set operator must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain the function block")
    };
    let FirExprKind::Block { statements, .. } = &body.expr(root).unwrap().kind else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(discard) = body.statement(statements[0]).unwrap().kind else {
        panic!("body must contain the set call")
    };
    let FirExprKind::ImplicitConversion { value, conversion } = &body.expr(discard).unwrap().kind
    else {
        panic!("indexed assignment must publish its result discard")
    };
    assert!(matches!(conversion.kind, FirConversionKind::CoerceToUnit));
    let call = body.expr(*value).expect("discarded call");
    assert_eq!(call.ty.get(), Ty::String);
    assert!(matches!(call.kind, FirExprKind::Call(_)));
}

#[test]
fn selected_statement_inc_operator_is_a_stable_fir_call_before_writeback() {
    let analysis = checked_analysis(
        "class Counter { operator fun inc(): Counter = this }\n\
         fun change(counter: Counter) { var current = counter; current++ }\n",
    );
    let file = &analysis.files[0];
    let function = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == "change" => Some(function),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("change declaration");
    let root = match function.body {
        crate::ast::FunBody::Block(root) => root,
        crate::ast::FunBody::Expr(_) | crate::ast::FunBody::None => {
            panic!("change must have a block body")
        }
    };
    let info = analysis.types[0].as_ref().expect("checked file");
    let parameter = CheckedBodyParameter {
        name: &function.params[0].name,
        ty: ResolvedTy::new(info.resolved_type(&function.params[0].ty).unwrap()).unwrap(),
        span: function.params[0].ty.span,
    };
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(67),
        root,
        &[parameter],
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("selected inc operator must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain the function block")
    };
    let FirExprKind::Block { statements, .. } = &body.expr(root).unwrap().kind else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Expression(write) = body.statement(statements[1]).unwrap().kind else {
        panic!("increment must write back")
    };
    let FirExprKind::ValueWrite { value, .. } = body.expr(write).unwrap().kind else {
        panic!("increment must be a value write")
    };
    assert!(matches!(
        body.expr(value).map(|expression| &expression.kind),
        Some(FirExprKind::Call(call)) if call.dispatch_receiver.is_some()
    ));
}

#[test]
fn custom_in_range_keeps_both_selected_convention_calls() {
    let analysis = checked_analysis(
        "class Span { operator fun contains(value: Bound): Boolean = true }\n\
         class Bound { operator fun rangeTo(other: Bound): Span = Span() }\n\
         fun test(value: Bound, start: Bound, end: Bound) = value in start..end\n",
    );
    let file = &analysis.files[0];
    let function = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == "test" => Some(function),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("test declaration");
    let root = match function.body {
        crate::ast::FunBody::Expr(root) => root,
        crate::ast::FunBody::Block(_) | crate::ast::FunBody::None => {
            panic!("test must have an expression body")
        }
    };
    let info = analysis.types[0].as_ref().expect("checked file");
    let parameters = function
        .params
        .iter()
        .map(|parameter| CheckedBodyParameter {
            name: &parameter.name,
            ty: ResolvedTy::new(info.resolved_type(&parameter.ty).unwrap()).unwrap(),
            span: parameter.ty.span,
        })
        .collect::<Vec<_>>();
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(71),
        root,
        &parameters,
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("custom membership must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain membership FIR")
    };
    let FirExprKind::ContainmentCall { call, .. } = &body.expr(root).unwrap().kind else {
        panic!("membership must retain the contains call")
    };
    let range = call
        .dispatch_receiver
        .expect("contains must dispatch on the selected range result")
        .value;
    assert!(matches!(
        body.expr(range).map(|expression| &expression.kind),
        Some(FirExprKind::Call(range_to)) if range_to.dispatch_receiver.is_some()
    ));
}

#[test]
fn floating_point_membership_carries_its_checked_comparison_type() {
    let analysis = checked_analysis("fun test(value: Double): Boolean = value in 1.0..3.0\n");
    let file = &analysis.files[0];
    let function = file
        .decls
        .iter()
        .find_map(|declaration| match file.decl(*declaration) {
            Decl::Fun(function) if function.name == "test" => Some(function),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
        .expect("test declaration");
    let crate::ast::FunBody::Expr(root) = function.body else {
        panic!("test must have an expression body")
    };
    let info = analysis.types[0].as_ref().expect("checked file");
    let parameter = CheckedBodyParameter {
        name: &function.params[0].name,
        ty: ResolvedTy::new(info.resolved_type(&function.params[0].ty).unwrap()).unwrap(),
        span: function.params[0].ty.span,
    };
    let mut origins = OriginStore::default();
    let body = check_expression_body_with_parameters(
        file,
        info,
        SourceFileId::from_raw(0),
        BodyOwnerId::from_raw(72),
        root,
        &[parameter],
        analysis.streamed.as_ref().expect("Pass 1").module.index(),
        &mut origins,
    )
    .expect("floating-point membership must build checked FIR");

    let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind else {
        panic!("root must contain membership FIR")
    };
    assert!(matches!(
        &body.expr(root).unwrap().kind,
        FirExprKind::InRange { comparison, .. } if comparison.get() == Ty::Double
    ));
}

#[test]
fn statically_proven_suspend_function_tests_and_casts_build_checked_fir() {
    let analysis = checked_analysis(
        "suspend fun test(c: suspend () -> String): Boolean = c is suspend () -> String\n\
         suspend fun cast(c: suspend () -> String): String = (c as suspend () -> String)()\n",
    );
    let file = &analysis.files[0];
    let info = analysis.types[0].as_ref().expect("checked file");
    let index = analysis.streamed.as_ref().expect("Pass 1").module.index();

    for (ordinal, name) in ["test", "cast"].into_iter().enumerate() {
        let function = file
            .decls
            .iter()
            .find_map(|declaration| match file.decl(*declaration) {
                Decl::Fun(function) if function.name == name => Some(function),
                Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
            })
            .expect("function declaration");
        let crate::ast::FunBody::Expr(root) = function.body else {
            panic!("{name} must have an expression body")
        };
        let parameter = CheckedBodyParameter {
            name: &function.params[0].name,
            ty: ResolvedTy::new(info.resolved_type(&function.params[0].ty).unwrap()).unwrap(),
            span: function.params[0].ty.span,
        };
        let mut origins = OriginStore::default();
        let body = check_expression_body_with_parameters(
            file,
            info,
            SourceFileId::from_raw(0),
            BodyOwnerId::from_raw(80 + ordinal as u32),
            root,
            &[parameter],
            index,
            &mut origins,
        )
        .expect("type operation must build checked FIR");
        let FirStatementKind::Expression(root) = body.statement(body.roots()[0]).unwrap().kind
        else {
            panic!("root must be expression FIR")
        };

        match name {
            "test" => assert!(matches!(
                body.expr(root).map(|expression| &expression.kind),
                Some(FirExprKind::TypeOperation {
                    operation: FirTypeOperation::Is,
                    target,
                    ..
                }) if matches!(target.get(), crate::types::Ty::Fun(signature) if signature.suspend)
            )),
            "cast" => {
                let Some(FirExprKind::FunctionInvoke {
                    callee, suspend, ..
                }) = body.expr(root).map(|expression| &expression.kind)
                else {
                    panic!("cast result must be a checked function invocation")
                };
                assert!(*suspend);
                assert!(matches!(
                    body.expr(*callee).map(|expression| &expression.kind),
                    Some(FirExprKind::TypeOperation {
                        operation: FirTypeOperation::Cast,
                        target,
                        ..
                    }) if matches!(target.get(), crate::types::Ty::Fun(signature) if signature.suspend)
                ));
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn named_suspend_function_tests_preserve_the_reflective_runtime_boundary() {
    let source = r#"
import kotlin.coroutines.Continuation
import kotlin.coroutines.SuspendFunction1
import kotlin.reflect.KSuspendFunction1

suspend fun target(value: Int): Int = value

suspend fun inspect(): Boolean {
    val reference = ::target
    val lambda = suspend { value: Int -> value }
    return reference is KSuspendFunction1<Int, Any?> &&
        reference is SuspendFunction1<Int, Any?> &&
        lambda !is KSuspendFunction1<Int, Any?> &&
        lambda is Function2<Int, Continuation<Int>, Any?>
}
"#;
    let mut diagnostics = DiagSink::new();
    let platform = crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
        crate::jvm::classpath::Classpath::new(crate::toolchain::classpath_jars_for(
            "// WITH_REFLECT",
        )),
    ));
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("SuspendClassifierTests")],
        Box::new(platform),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let file = &analysis.files[0];
    let info = analysis.types[0].as_ref().expect("checked file");
    let runtime_targets = file
        .expr_arena
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            matches!(expression, Expr::Is { .. })
                .then(|| info.expr_lowers.get(&crate::ast::ExprId(index as u32)))
                .flatten()
        })
        .filter_map(|lowering| match lowering {
            crate::resolve::ExprLowering::RuntimeTypeOperand(target) => Some(*target),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(runtime_targets.len(), 4);
    assert!(matches!(
        runtime_targets[0],
        Ty::Obj(name, _) if name.matches("kotlin/reflect/KSuspendFunction1")
    ));
    assert!(matches!(
        runtime_targets[1],
        Ty::Fun(signature) if signature.suspend && signature.params.len() == 1
    ));
    assert!(matches!(
        runtime_targets[2],
        Ty::Obj(name, _) if name.matches("kotlin/reflect/KSuspendFunction1")
    ));
    assert!(matches!(
        runtime_targets[3],
        Ty::Fun(signature) if !signature.suspend && signature.params.len() == 2
    ));
}
