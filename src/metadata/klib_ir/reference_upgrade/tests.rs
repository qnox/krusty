use super::*;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::KlibIrVariance;

fn class(name: &str) -> KlibIrSymbol {
    KlibIrSymbol {
        kind: KlibIrSymbolKind::Class,
        signature: KlibIrSignature::Public(KlibPublicIdSignature::classifier(
            crate::types::type_name(name),
        )),
    }
}

fn simple(arena: &mut KlibIrArena, name: &str, arguments: Vec<KlibIrTypeArgument>) -> KlibIrTypeId {
    arena.push_type(KlibIrType::Simple {
        classifier: class(name),
        nullability: KlibIrNullability::NotSpecified,
        arguments,
        annotations: Vec::new(),
        abbreviation: None,
    })
}

fn argument(ty: KlibIrTypeId) -> KlibIrTypeArgument {
    KlibIrTypeArgument::Type {
        variance: KlibIrVariance::Invariant,
        ty,
    }
}

/// A target function, `kotlin/Int` and `kotlin/reflect/<family><Int..., Int>`.
struct Fixture {
    arena: KlibIrArena,
    target: KlibIrSymbol,
    int: KlibIrTypeId,
    reference: KlibIrTypeId,
}

fn fixture(family: &str, arity: usize) -> Fixture {
    let mut arena = KlibIrArena::default();
    let int = simple(&mut arena, "kotlin/Int", Vec::new());
    let reference = simple(
        &mut arena,
        &format!("kotlin/reflect/{family}{arity}"),
        (0..=arity).map(|_| argument(int)).collect(),
    );
    let target = KlibIrSymbol {
        kind: KlibIrSymbolKind::Function,
        signature: KlibIrSignature::Public(
            KlibPublicIdSignature::classifier(crate::types::type_name("fixture/Box"))
                .member_function("step", "step(kotlin.Int){}kotlin.Int"),
        ),
    };
    Fixture {
        arena,
        target,
        int,
        reference,
    }
}

fn access(target: &KlibIrSymbol, arguments: KlibIrArguments) -> KlibIrMemberAccess {
    KlibIrMemberAccess {
        symbol: target.clone(),
        arguments,
        type_arguments: Vec::new(),
        origin: None,
    }
}

fn split(
    dispatch_receiver: Option<KlibIrExprId>,
    extension_receiver: Option<KlibIrExprId>,
    values: Vec<Option<KlibIrExprId>>,
) -> KlibIrArguments {
    KlibIrArguments::Split {
        dispatch_receiver,
        extension_receiver,
        values,
    }
}

fn locals() -> impl FnMut() -> KlibIrSignature {
    let mut next = u32::MAX;
    move || {
        next -= 1;
        KlibIrSignature::FileLocal {
            file: 9,
            slot: next + 1,
        }
    }
}

fn invoke_member(arity: usize, class: &str) -> KlibIrSymbol {
    let operands = (0..arity)
        .map(|index| format!("1:{index}"))
        .collect::<Vec<_>>()
        .join(";");
    KlibIrSymbol {
        kind: KlibIrSymbolKind::Function,
        signature: KlibIrSignature::Public(
            KlibPublicIdSignature::classifier(crate::types::type_name(class))
                .member_function("invoke", &format!("invoke({operands}){{}}")),
        ),
    }
}

/// The parameter types of `invoke`, and the arguments its forwarding call passes, which must
/// read those parameters in order.
fn forwarding(
    arena: &KlibIrArena,
    invoke: KlibIrFunctionId,
) -> (Vec<KlibIrTypeId>, KlibIrMemberAccess) {
    let function = arena.function(invoke);
    let Some(KlibIrBody::Block(statements)) = &function.body else {
        panic!("invoke has a block body: {:?}", function.body);
    };
    let [KlibIrStatement::Expression(returned)] = statements.as_slice() else {
        panic!("invoke returns one call: {statements:?}");
    };
    let KlibIrExprKind::Return { target, value } = &arena.expr(*returned).kind else {
        panic!("invoke returns: {:?}", arena.expr(*returned));
    };
    assert_eq!(target, &function.base.symbol);
    let KlibIrExprKind::Call {
        access,
        super_qualifier: None,
    } = &arena.expr(*value).kind
    else {
        panic!("invoke calls its target: {:?}", arena.expr(*value));
    };
    assert_eq!(arena.expr(*value).ty, Some(function.return_type));
    let KlibIrArguments::Flat(arguments) = &access.arguments else {
        panic!("one argument per parameter: {:?}", access.arguments);
    };
    assert_eq!(arguments.len(), function.regular_parameters.len());
    for (argument, parameter) in arguments.iter().zip(&function.regular_parameters) {
        let read = arena.expr(argument.expect("every parameter is forwarded"));
        assert_eq!(read.ty, Some(parameter.ty));
        assert_eq!(
            read.kind,
            KlibIrExprKind::GetValue {
                symbol: parameter.base.symbol.clone(),
                origin: None,
            }
        );
    }
    (
        function
            .regular_parameters
            .iter()
            .map(|parameter| parameter.ty)
            .collect(),
        access.clone(),
    )
}

#[test]
fn an_unbound_reference_becomes_an_invoke_function_forwarding_every_parameter() {
    let Fixture {
        mut arena,
        target,
        int,
        reference,
    } = fixture("KFunction", 2);
    let legacy = access(&target, split(None, None, vec![None]));
    let rich = upgrade(&mut arena, Some(reference), &legacy, None, &mut locals())
        .expect("a plain reference upgrades");
    let KlibIrExprKind::RichFunctionReference {
        bound_values,
        reflection_target,
        overridden,
        invoke,
        flags,
        origin,
    } = rich
    else {
        panic!("{rich:?}");
    };
    assert_eq!(bound_values, Vec::new());
    assert_eq!(reflection_target, Some(target.clone()));
    assert_eq!(overridden, invoke_member(2, "kotlin/reflect/KFunction2"));
    assert_eq!((flags, origin), (0, None));
    let function = arena.function(invoke);
    assert_eq!(function.name, "step");
    assert_eq!(function.base.origin, INVOKE_ORIGIN);
    assert_eq!(function.return_type, int);
    assert_eq!(
        function
            .regular_parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect::<Vec<_>>(),
        ["p0", "p1"]
    );
    let (parameters, call) = forwarding(&arena, invoke);
    assert_eq!(parameters, [int, int]);
    assert_eq!(call.symbol, target);
}

#[test]
fn a_bound_receiver_becomes_the_first_bound_value_and_parameter() {
    for receiver in [true, false] {
        let Fixture {
            mut arena,
            target,
            int,
            reference,
        } = fixture("KFunction", 1);
        let receiver_type = simple(&mut arena, "fixture/Box", Vec::new());
        let bound = arena.push_expr(
            Some(receiver_type),
            KlibIrExprKind::Error {
                description: "receiver".to_owned(),
            },
        );
        let arguments = if receiver {
            split(Some(bound), None, vec![None])
        } else {
            split(None, Some(bound), vec![None])
        };
        let reflected = KlibIrSymbol {
            kind: KlibIrSymbolKind::Function,
            signature: KlibIrSignature::FileLocal { file: 9, slot: 3 },
        };
        let rich = upgrade(
            &mut arena,
            Some(reference),
            &access(&target, arguments),
            Some(&reflected),
            &mut locals(),
        )
        .expect("a reference with one bound receiver upgrades");
        let KlibIrExprKind::RichFunctionReference {
            bound_values,
            reflection_target,
            invoke,
            ..
        } = rich
        else {
            panic!("{rich:?}");
        };
        assert_eq!(bound_values, [bound]);
        assert_eq!(reflection_target, Some(reflected));
        assert_eq!(
            arena.function(invoke).regular_parameters[0].name,
            "receiver"
        );
        assert_eq!(forwarding(&arena, invoke).0, [receiver_type, int]);
    }
}

#[test]
fn a_reference_that_is_not_plain_keeps_its_serialized_form() {
    let Fixture {
        mut arena,
        target,
        reference,
        ..
    } = fixture("KFunction", 1);
    let value = arena.push_expr(
        None,
        KlibIrExprKind::Error {
            description: "value".to_owned(),
        },
    );
    let mut adapted = access(&target, split(None, None, vec![None]));
    adapted.origin = Some("ADAPTED_FUNCTION_REFERENCE".to_owned());
    let two_receivers = access(&target, split(Some(value), Some(value), vec![None]));
    let regular = access(&target, split(None, None, vec![Some(value)]));
    for (case, legacy) in [
        ("an adapted reference", adapted),
        ("two bound receivers", two_receivers),
        ("a bound regular argument", regular),
    ] {
        assert_eq!(
            upgrade(&mut arena, Some(reference), &legacy, None, &mut locals()),
            None,
            "{case}"
        );
    }
    let plain = access(&target, split(None, None, vec![None]));
    let suspend = fixture("KSuspendFunction", 1);
    let function = fixture("Function", 1);
    for (case, mut fixture) in [
        ("a suspend reference", suspend),
        ("a function type", function),
    ] {
        assert_eq!(
            upgrade(
                &mut fixture.arena,
                Some(fixture.reference),
                &plain,
                None,
                &mut locals()
            ),
            None,
            "{case}"
        );
    }
    let star = simple(
        &mut arena,
        "kotlin/reflect/KFunction0",
        vec![KlibIrTypeArgument::Star],
    );
    assert_eq!(
        upgrade(&mut arena, Some(star), &plain, None, &mut locals()),
        None
    );
    assert_eq!(upgrade(&mut arena, None, &plain, None, &mut locals()), None);
}

/// Kotlin 2.4 serializes references in the rich form itself. Upgrading the plain reference an
/// older producer would have written for each of them yields the same reference: the same bound
/// values, reflected declaration and implemented `invoke` member, and an `invoke` function of the
/// same parameter types forwarding them in the same order.
#[test]
fn an_upgraded_reference_matches_the_rich_form_kotlin_2_4_serializes() {
    let Some(root) = std::env::var_os("KRUSTY_KOTLIN_NATIVE").filter(|value| !value.is_empty())
    else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "KRUSTY_REQUIRE_KLIB is set but KRUSTY_KOTLIN_NATIVE is not"
        );
        return;
    };
    let work =
        std::env::temp_dir().join(format!("krusty-reference-upgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let source = work.join("References.kt");
    std::fs::write(
        &source,
        "package fixture\n\
         class Box(val base: Int) {\n\
             fun step(value: Int): Int = base + value\n\
             private fun hidden(value: Int, other: String): Int = value + other.length\n\
             fun unbound(): (Box, Int) -> Int = Box::step\n\
             fun bound(): (Int) -> Int = this::step\n\
             fun private(): (Box, Int, String) -> Int = Box::hidden\n\
         }\n\
         fun Int.twice(): Int = this * 2\n\
         fun top(value: Int, other: Long): Long = value + other\n\
         fun topLevel(): (Int, Long) -> Long = ::top\n\
         fun extension(): (Int) -> Int = Int::twice\n\
         fun boundExtension(): () -> Int = 3::twice\n",
    )
    .unwrap();
    let library = work.join("references");
    let output = std::process::Command::new(std::path::Path::new(&root).join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .expect("run the Native producer");
    assert!(
        output.status.success(),
        "producer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut trees = crate::metadata::klib_ir::read_declaration_trees(
        &crate::klib::KlibArchive::open(&library).expect("the library opens"),
    )
    .expect("the library decodes")
    .into_trees();
    let _ = std::fs::remove_dir_all(&work);
    let mut compared = Vec::new();
    for tree in &mut trees {
        let arena = &mut tree.arena;
        let references = (0..arena.expr_count())
            .map(|index| arena.expr_id(index))
            .filter(|id| {
                matches!(
                    arena.expr(*id).kind,
                    KlibIrExprKind::RichFunctionReference { .. }
                )
            })
            .collect::<Vec<_>>();
        for id in references {
            let expression = arena.expr(id).clone();
            let KlibIrExprKind::RichFunctionReference {
                bound_values,
                reflection_target,
                overridden,
                invoke,
                ..
            } = &expression.kind
            else {
                unreachable!("selected by kind");
            };
            let (parameters, call) = forwarding(arena, *invoke);
            // Which receiver the reference binds is what a pre-2.4 producer recorded; the rich
            // form does not depend on it.
            let receiver = bound_values.first().copied();
            let arguments = split(receiver, None, Vec::new());
            let legacy = access(&call.symbol, arguments);
            let rich = upgrade(
                arena,
                expression.ty,
                &legacy,
                reflection_target.as_ref(),
                &mut locals(),
            )
            .expect("the plain form of a 2.4 reference upgrades");
            let KlibIrExprKind::RichFunctionReference {
                bound_values: upgraded_bound,
                reflection_target: upgraded_target,
                overridden: upgraded_overridden,
                invoke: upgraded_invoke,
                flags,
                origin,
            } = rich
            else {
                panic!("{rich:?}");
            };
            assert_eq!(&upgraded_bound, bound_values);
            assert_eq!(&upgraded_target, reflection_target);
            assert_eq!(&upgraded_overridden, overridden);
            assert_eq!((flags, origin), (0, None));
            let (upgraded_parameters, upgraded_call) = forwarding(arena, upgraded_invoke);
            assert_eq!(upgraded_parameters, parameters);
            assert_eq!(upgraded_call.symbol, call.symbol);
            assert_eq!(
                arena.function(upgraded_invoke).name,
                arena.function(*invoke).name
            );
            compared.push(arena.function(*invoke).name.clone());
        }
    }
    compared.sort();
    assert_eq!(
        compared,
        ["hidden", "step", "step", "top", "twice", "twice"]
    );
}
