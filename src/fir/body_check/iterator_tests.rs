use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_stdlib_semantics,
    root_expression,
};
use super::*;
use crate::fir::FirRangeCounterKind;

#[test]
fn source_iterator_loop_keeps_all_selected_convention_targets() {
    let (body, index) = checked_function_body(
        "class WordsIterator {\n\
             operator fun hasNext(): Boolean = false\n\
             operator fun next(): String = \"word\"\n\
         }\n\
         class Words { operator fun iterator(): WordsIterator = WordsIterator() }\n\
         fun run(words: Words) { for (word in words) { word } }\n",
        "run",
    );
    let FirExprKind::Block { statements, .. } = &body.expr(root_expression(&body)).unwrap().kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Loop {
        header:
            FirLoopHeader::Iterator {
                iterator,
                has_next,
                next,
                iterator_ty,
                variable_ty,
                ..
            },
        ..
    } = &body.statement(statements[0]).unwrap().kind
    else {
        panic!("custom for-loop must retain its iterator protocol")
    };
    for call in [iterator, has_next, next] {
        let FirCallTarget::Module(target) = call.target else {
            panic!("source iterator convention must retain its module identity")
        };
        assert!(index.callable(target).is_some());
    }
    assert_eq!(variable_ty.get(), crate::types::Ty::String);
    assert_eq!(
        iterator_ty
            .get()
            .obj_internal()
            .map(|name| name.segment_ref()),
        Some("WordsIterator")
    );
}

#[test]
fn context_extension_iterator_keeps_its_checked_context_operand() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +ContextReceivers\n\
         class Context\n\
         class Values\n\
         class ValuesIterator {\n\
             operator fun hasNext(): Boolean = false\n\
             operator fun next(): Int = 1\n\
         }\n\
         context(Context)\n\
         operator fun Values.iterator(): ValuesIterator = ValuesIterator()\n\
         context(Context)\n\
         fun run() { for (value in Values()) { value } }\n",
        "run",
    );
    let FirExprKind::Block { statements, .. } = &body.expr(root_expression(&body)).unwrap().kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Loop {
        header: FirLoopHeader::Iterator { iterator, .. },
        ..
    } = &body.statement(statements[0]).unwrap().kind
    else {
        panic!("context extension must remain a checked iterator call")
    };
    assert_eq!(iterator.context_arguments.len(), 1);
    assert!(matches!(
        body.expr(iterator.context_arguments[0].receiver.value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver { .. })
    ));
}

#[test]
fn member_extension_next_keeps_both_iterator_protocol_receivers() {
    let (body, index) = checked_function_body(
        "class It {\n\
             operator fun hasNext(): Boolean = false\n\
         }\n\
         class C { operator fun iterator(): It = It() }\n\
         class X {\n\
             operator fun It.next(): Int = 5\n\
             fun run() { for (value in C()) { value } }\n\
         }\n",
        "run",
    );
    let FirExprKind::Block { statements, .. } = &body.expr(root_expression(&body)).unwrap().kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Loop {
        header: FirLoopHeader::Iterator { next, .. },
        ..
    } = &body.statement(statements[0]).unwrap().kind
    else {
        panic!("member-extension next must remain a checked iterator loop")
    };
    let FirCallTarget::Module(target) = next.target else {
        panic!("source member extension must retain its module identity")
    };
    assert!(index.callable(target).is_some());
    let crate::fir::FirIteratorReceiver::MemberExtension { dispatch_receiver } = &next.receiver
    else {
        panic!("member extension must retain dispatch and extension receiver placement")
    };
    assert!(matches!(
        body.expr(dispatch_receiver.value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver { .. })
    ));
}

#[test]
fn user_defined_range_loop_keeps_range_and_iterator_selections() {
    let (body, index) = checked_function_body(
        "class Cursor {\n\
             operator fun hasNext(): Boolean = false\n\
             operator fun next(): Point = Point()\n\
         }\n\
         class Points { operator fun iterator(): Cursor = Cursor() }\n\
         class Point { operator fun rangeTo(other: Point): Points = Points() }\n\
         fun run(first: Point, last: Point) { for (point in first..last) { point } }\n",
        "run",
    );
    let FirExprKind::Block { statements, .. } = &body.expr(root_expression(&body)).unwrap().kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Loop {
        header:
            FirLoopHeader::Iterator {
                iterable,
                iterator,
                has_next,
                next,
                variable_ty,
                ..
            },
        ..
    } = &body.statement(statements[0]).unwrap().kind
    else {
        panic!("a user-defined range must publish an iterator loop")
    };
    let FirExprKind::Call(range) = &body.expr(*iterable).expect("range call").kind else {
        panic!("the iterable must be the checked rangeTo call")
    };
    for target in [
        &range.target,
        &iterator.target,
        &has_next.target,
        &next.target,
    ] {
        let FirCallTarget::Module(callable) = target else {
            panic!("source convention must retain a module callable identity")
        };
        assert!(index.callable(*callable).is_some());
    }
    assert_eq!(
        variable_ty
            .get()
            .obj_internal()
            .map(|name| name.segment_ref()),
        Some("Point")
    );
}

#[test]
fn range_loop_commits_a_platform_integer_bound_to_its_checked_lower_bound() {
    let Some(jdk) = crate::toolchain::jdk_modules() else {
        return;
    };
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB");
    classpath.push(jdk);
    let platform = Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
        std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
    ));
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         fun run(values: ArrayList<Int>) {\n\
             for (value in values[0]..3) { value }\n\
        }\n",
        "run",
        platform,
    );
    let FirExprKind::Block { statements, .. } = &body.expr(root_expression(&body)).unwrap().kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Loop {
        header: FirLoopHeader::Range { counter, .. },
        ..
    } = &body.statement(statements[0]).unwrap().kind
    else {
        panic!("platform integer bounds must form a checked range loop")
    };
    assert_eq!(*counter, FirRangeCounterKind::Int);
}

#[test]
fn unsigned_down_to_keeps_the_selected_infix_range_declaration() {
    let (body, _) = checked_function_body_with_platform(
        "fun run(first: UByte, last: UByte) {\n\
             for (value in first downTo last) { value }\n\
         }\n",
        "run",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Block { statements, .. } = &body.expr(root_expression(&body)).unwrap().kind
    else {
        panic!("function body must be a FIR block")
    };
    let FirStatementKind::Loop {
        header:
            FirLoopHeader::Iterator {
                variable_ty,
                iterable,
                ..
            },
        ..
    } = &body.statement(statements[0]).unwrap().kind
    else {
        panic!("UByte.downTo must retain its checked progression and iterator protocol")
    };
    let range = body.expr(*iterable).expect("checked downTo call");
    assert_eq!(
        range.ty.get(),
        crate::types::Ty::obj("kotlin/ranges/UIntProgression")
    );
    assert_eq!(variable_ty.get(), crate::types::Ty::UInt);
    let FirExprKind::Call(call) = &range.kind else {
        panic!("downTo must be a selected semantic call")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert!(call.extension_receiver.is_some());
    assert_eq!(call.arguments.len(), 1);
}
