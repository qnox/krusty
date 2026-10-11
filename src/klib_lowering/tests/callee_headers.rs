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
use crate::libraries::{ExternalCallableKind, KlibDeclarationSignature};
use crate::metadata::id_signature::{
    callable_signature, CallableShape, DeclarationContainer, KlibPublicIdSignature, Placement,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrBody, KlibIrExprKind, KlibIrFunction, KlibIrMember, KlibIrParameter,
    KlibIrStatement,
};
use crate::metadata::klib_ir::{KlibIrDeclarationTree, KlibIrSymbolKind};
use crate::metadata::semantic::KotlinType;
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
fn a_header_declared_callee_without_a_body_declines_by_its_own_reason() {
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

/// A provider record of `demo.b(a: Int): Int` with one fact changed.
struct Record {
    name: &'static str,
    params: [Ty; 1],
    identities: [ResolvedParameterIdentity; 1],
    ret: Ty,
    signature: KlibDeclarationSignature,
}

impl Record {
    fn of_b(demo: &Demo) -> Self {
        Self {
            name: "b",
            params: [T::Int.semantic()],
            identities: [ResolvedParameterIdentity::Source("a".into())],
            ret: T::Int.semantic(),
            signature: demo.facts["b"]
                .declaration_signature
                .clone()
                .expect("signed"),
        }
    }

    fn callable(&self) -> KlibCallable<'_> {
        KlibCallable::new(
            self.name,
            ExternalCallableKind::TopLevel,
            &self.params,
            self.ret,
            &self.identities,
            0,
            Some(&self.signature),
        )
        .expect("signed")
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
    let nothing = arena.push_type(T::Nothing.klib());
    let mut slot = 9000;
    let mut parameter = |kind, name: &str| {
        slot += 1;
        KlibIrParameter {
            base: base(local_symbol(kind, slot)),
            name: name.to_owned(),
            ty: int,
            vararg_element_type: None,
            default_value: None,
        }
    };
    let dispatch_receiver =
        dispatch.then(|| parameter(KlibIrSymbolKind::ReceiverParameter, "<this>"));
    let context_parameters = (0..contexts)
        .map(|_| parameter(KlibIrSymbolKind::ValueParameter, "c"))
        .collect::<Vec<_>>();
    let regular_parameters = (0..values)
        .map(|_| parameter(KlibIrSymbolKind::ValueParameter, "v"))
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

#[test]
fn a_call_no_loaded_library_declares_declines_by_name() {
    let undeclared = demo_signature(None, "z", &[], &[T::Int]);
    let demo = demo(&CALLER, |_, body| {
        let a = body.param(0);
        let call = body.call_signature(undeclared.clone(), vec![a], None, T::Int);
        Some(vec![body.ret(call)])
    });
    assert_eq!(
        demo.declined("a", &[]),
        "the KLIB body of `demo.a` (it calls `demo.z`, which no loaded library declares)"
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
                callee: crate::ir::Callee::External {
                    target,
                    default_provider: None,
                    params: function.callable.params.clone(),
                    ret: function.callable.ret,
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                    extension_receiver_parameter: None,
                },
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
    let (libraries, bodies) = super::stdlib(&root);
    let mut reached = std::collections::BTreeMap::<String, usize>::new();
    let mut lowered_with_callees = Vec::new();
    let mut undeclared = Vec::new();
    for (package, record) in stdlib_records(&root, &libraries) {
        let mut unit = DependencyBodyUnit::default();
        let callable = record.klib_body_callable().expect("signed");
        match unit.lower_function(callable, &bodies, &KlibCalleeFacts::default()) {
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
    assert_eq!(
        reached.into_iter().collect::<Vec<_>>(),
        [
            ("it declares a parameter default)", 19),
            ("it uses a block of a compiler-introduced form)", 34),
            ("it uses a call of a file-private declaration)", 12),
            ("it uses a call of a member function)", 36),
            ("it uses a constructor call)", 2),
            ("it uses a type with type arguments)", 6),
            ("its library serializes none)", 51),
        ]
        .map(|(reason, count)| (reason.to_owned(), count))
    );
}

/// Bodies that declined at their first call before callees were declared from their headers, and
/// now decline inside the callee.
#[test]
fn stdlib_bodies_decline_inside_header_declared_callees() {
    let Some(root) = super::distribution_root() else {
        return;
    };
    let (libraries, bodies) = super::stdlib(&root);
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
             `kotlin.collections.indexOf` (it uses a block of a compiler-introduced form))",
        ),
    ];
    let declines = cases
        .iter()
        .map(|(record, _)| {
            let mut unit = DependencyBodyUnit::default();
            match unit.lower_function(
                record.klib_body_callable().expect("signed"),
                &bodies,
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
