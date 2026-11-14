//! A getter leaf authenticates its property slot and complete declaring-class receiver.

use super::*;
use crate::libraries::{ExternalCallableRealization, KlibAccessorRole};
use crate::metadata::klib_ir::tree::{KlibIrProperty, KlibIrTypeArgument, KlibIrVariance};

struct ProducerDirectory(std::path::PathBuf);

impl ProducerDirectory {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "krusty-klib-accessor-{}-{ordinal}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create accessor producer directory: {error}"),
            }
        }
    }
}

impl Drop for ProducerDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy, Debug)]
enum Change {
    None,
    ReceiverMask,
    ReceiverKind,
    ReceiverApplication,
    PropertyGroup,
    SetterSlot,
    Result,
}

fn pair_getter(libraries: &KlibLibraries) -> ExternalCallableRealization {
    let class = libraries.classifier(type_name("kotlin/Pair")).unwrap();
    let getter = &class.declared_callables["second"].properties()[0].getter;
    libraries
        .external_callable(getter.external_identity.unwrap())
        .unwrap()
}

/// Retain the real selected declaration and rebuild a body-free header in a fresh arena. No
/// executable implementation is available to hide a malformed property or receiver contract.
fn getter_header(
    libraries: &KlibLibraries,
    provider: &ExternalCallableRealization,
    change: Change,
) -> KlibDeclarationBodies {
    let selected = provider.klib_body_callable().unwrap();
    let contract = selected.class_contract().unwrap();
    let raw = libraries
        .declaration_bodies()
        .class_member_header(
            selected.identity(),
            &contract.signature,
            Some(crate::libraries::KlibAccessorRole::Getter),
        )
        .unwrap()
        .unwrap();
    let mut class = raw.class.clone();
    let mut function = raw.function.clone();
    let mut arena = KlibIrArena::default();
    let any = arena.push_type(super::demo_package::T::NullableAny.klib());
    assert_eq!(class.type_parameters.len(), 2);
    let formals = class
        .type_parameters
        .iter_mut()
        .map(|parameter| {
            parameter.supertypes = vec![any];
            arena.push_type(KlibIrType::Simple {
                classifier: parameter.base.symbol.clone(),
                nullability: KlibIrNullability::NotSpecified,
                arguments: Vec::new(),
                annotations: Vec::new(),
                abbreviation: None,
            })
        })
        .collect::<Vec<_>>();
    let receiver_signature = if matches!(change, Change::ReceiverMask) {
        super::constructor_contracts::different_mask(&contract.signature)
    } else {
        contract.signature.clone()
    };
    let receiver = arena.push_type(KlibIrType::Simple {
        classifier: public_symbol(
            if matches!(change, Change::ReceiverKind) {
                KlibIrSymbolKind::Function
            } else {
                KlibIrSymbolKind::Class
            },
            receiver_signature,
        ),
        nullability: KlibIrNullability::NotSpecified,
        arguments: [
            formals[0],
            if matches!(change, Change::ReceiverApplication) {
                formals[0]
            } else {
                formals[1]
            },
        ]
        .into_iter()
        .map(|ty| KlibIrTypeArgument::Type {
            variance: KlibIrVariance::Invariant,
            ty,
        })
        .collect(),
        annotations: Vec::new(),
        abbreviation: None,
    });
    function.dispatch_receiver.as_mut().unwrap().ty = receiver;
    function.return_type = if matches!(change, Change::Result) {
        any
    } else {
        formals[1]
    };
    function.body = None;
    function.overridden.clear();
    let function = arena.push_function(function);
    let KlibDeclarationSignature::Accessor(accessor) = selected.signature() else {
        panic!("getter")
    };
    class.members = vec![KlibIrMember::Property(KlibIrProperty {
        base: base(public_symbol(
            KlibIrSymbolKind::Property,
            if matches!(change, Change::PropertyGroup) {
                contract.signature.clone()
            } else {
                accessor.property().clone()
            },
        )),
        name: "second".to_owned(),
        backing_field: None,
        getter: (!matches!(change, Change::SetterSlot)).then_some(function),
        setter: matches!(change, Change::SetterSlot).then_some(function),
    })];
    class.supertypes.clear();
    if let Some(this) = &mut class.this_receiver {
        this.ty = receiver;
    }
    let class = arena.push_class(class);
    KlibDeclarationBodies::from_libraries(vec![(
        contract.module.clone(),
        KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Class(class),
        }])
        .unwrap(),
    )])
    .unwrap()
}

#[test]
fn getter_leaves_reject_malformed_class_and_property_headers() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let provider = pair_getter(&libraries);
    let selected = provider.klib_body_callable().unwrap();
    let linked = super::super::accessor_calls::authenticate(
        selected,
        &getter_header(&libraries, &provider, Change::None),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        linked.target,
        super::super::body_lowering::LinkedAccessorTarget::External(property)
            if property == selected.accessor().unwrap().property
    ));
    assert_eq!(linked.ret, selected.ret());
    assert!(!linked.write);
    for change in [
        Change::ReceiverMask,
        Change::ReceiverKind,
        Change::ReceiverApplication,
        Change::PropertyGroup,
        Change::SetterSlot,
        Change::Result,
    ] {
        assert!(
            matches!(
                super::super::accessor_calls::authenticate(
                    selected,
                    &getter_header(&libraries, &provider, change)
                ),
                Err(KlibBodyDeclineReason::SignatureMismatch(_))
            ),
            "{change:?}"
        );
    }
}

#[test]
fn getter_facets_cannot_turn_public_functions_or_setters_into_getters() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let provider = pair_getter(&libraries);
    let bodies = getter_header(&libraries, &provider, Change::None);
    let mut public = provider.clone();
    let signature = public
        .klib_body_class_contract
        .as_ref()
        .unwrap()
        .signature
        .clone();
    public.dependency_callable.as_mut().unwrap().declaration =
        KlibDeclarationSignature::Public(signature.clone());
    public.declaration_signature = Some(KlibDeclarationSignature::Public(signature));
    assert!(
        matches!(super::super::accessor_calls::authenticate(public.klib_body_callable().unwrap(), &bodies),
        Err(KlibBodyDeclineReason::SignatureMismatch(ref reason))
            if reason == "a getter contract belongs to a non-accessor declaration")
    );
    let mut setter = provider.clone();
    setter.klib_body_accessor.as_mut().unwrap().role = KlibAccessorRole::Setter;
    assert!(super::super::accessor_calls::authenticate(
        setter.klib_body_callable().unwrap(),
        &bodies
    )
    .unwrap()
    .is_none());
}

#[test]
fn target_getter_capability_requires_a_unique_accessor_contract() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let provider = pair_getter(&libraries);
    let selected = provider.klib_body_callable().unwrap();
    let bodies = getter_header(&libraries, &provider, Change::None);
    let facts = KlibCalleeFacts::new([selected]);
    assert!(!facts
        .target_getter(selected.identity(), &bodies, &|_| false)
        .unwrap());
    assert!(facts
        .target_getter(selected.identity(), &bodies, &|_| true)
        .unwrap());
    for role_change in [false, true] {
        let mut other = provider.clone();
        let facet = other.klib_body_accessor.as_mut().unwrap();
        if role_change {
            facet.role = KlibAccessorRole::Setter;
        } else {
            facet.property = crate::fir::ExternalPropertyId::from_raw(facet.property.raw() + 1);
        }
        let ambiguous = KlibCalleeFacts::new([selected, other.klib_body_callable().unwrap()]);
        assert!(
            matches!(ambiguous.target_getter(selected.identity(), &bodies, &|_| true),
            Err(KlibBodyDeclineReason::SignatureMismatch(ref reason))
                if reason == "conflicting provider contracts for a getter declaration")
        );
    }
}

#[test]
fn a_generic_extension_getter_keeps_its_selected_formal_through_body_lowering() {
    let Some(root) = distribution_root() else {
        return;
    };
    let work = ProducerDirectory::new();
    let source = work.0.join("GenericExtension.kt");
    std::fs::write(
        &source,
        "package fixture\nval <T> T.echo: T get() = this\nfun read(value: String): String = value.echo\n",
    )
    .unwrap();
    let library = work.0.join("generic-extension");
    let output = std::process::Command::new(root.join("bin/kotlinc-native"))
        .args(["-p", "library", "-nopack", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .expect("run exact generic accessor producer");
    assert!(
        output.status.success(),
        "producer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let libraries = KlibLibraries::open(&[
        crate::klib_libraries::KlibLibraryInput::platform_stdlib(
            root.join("klib/common/stdlib"),
            crate::klib_libraries::KlibStdlibSemantics::Native,
        ),
        crate::klib_libraries::KlibLibraryInput::dependency(library),
    ])
    .unwrap();
    let property_symbols =
        libraries.symbols(SymbolNamespace::Package(type_name("fixture")), "echo");
    let [property] = property_symbols.callables.properties() else {
        panic!("one provider generic extension property")
    };
    let getter_id = property.getter.external_identity.expect("exact getter id");
    let getter = libraries
        .external_callable(getter_id)
        .expect("the provider realizes its getter");
    let selected_getter = getter.klib_body_callable().expect("signed getter");
    assert_eq!(
        selected_getter.kind(),
        crate::libraries::ExternalCallableKind::Extension
    );
    assert_eq!(selected_getter.type_parameters().len(), 1);

    let read_symbols = libraries.symbols(SymbolNamespace::Package(type_name("fixture")), "read");
    let [read] = read_symbols.callables.functions() else {
        panic!("one provider read function")
    };
    let read_id = read.callable.external_identity.expect("exact read id");
    let read = libraries
        .external_callable(read_id)
        .expect("the provider realizes read");
    let mut unit = super::super::DependencyBodyUnit::default();
    let mut externals = crate::fx_hash::HashMap::default();
    externals.insert(
        selected_getter.identity().clone(),
        super::super::EmittedDependencyCallable::new(getter_id, None, None),
    );
    let function = unit
        .lower_function_with_declarations(
            read.klib_body_callable().expect("signed read"),
            libraries.declaration_bodies(),
            &super::super::KlibCalleeFacts::new([selected_getter]),
            crate::klib_libraries::KlibIntrinsicFacts::empty(),
            &externals,
            &libraries,
        )
        .unwrap_or_else(|decline| panic!("generic extension property body: {decline}"));
    super::validated(unit.ir());
    assert!(unit.ir().functions[function as usize].body.is_some());
    let reads = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            crate::ir::IrExpr::Checked(operation) => match operation.as_ref() {
                crate::ir::IrCheckedOperation::ExternalPropertyRead {
                    target,
                    substitutions,
                    ..
                } => Some((*target, substitutions.as_slice())),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(target, substitutions)] = reads.as_slice() else {
        panic!("one checked generic extension property read: {reads:?}")
    };
    assert_eq!(*target, property.getter.external_property_identity.unwrap());
    assert_eq!(substitutions.len(), 1);
    assert_eq!(
        substitutions[0].parameter,
        crate::fir::FirTypeParameterRef::External {
            callable: getter_id,
            ordinal: 0,
        }
    );
    assert_eq!(substitutions[0].value, Ty::String);
}
