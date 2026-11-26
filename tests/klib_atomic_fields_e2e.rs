//! PLATFORM-SPECIFIC: the Native stdlib KLIB's own `kotlin.concurrent.atomics` member bodies.
//!
//! Native's `AtomicInt`, `AtomicLong`, `AtomicBoolean` and `AtomicReference` are ordinary KLIB
//! classes. Each member body calls an external `kotlin.concurrent` function whose `TypedIntrinsic`
//! kind names one atomic operation, passing a property reference bound to the instance's own
//! backing field. `AtomicIntArray`, `AtomicLongArray` and `AtomicArray` hold an array instead, and
//! each member calls the `TypedIntrinsic` extension of that array naming its atomic element
//! operation. The JVM maps the same classes onto `java.util.concurrent.atomic` and never reads
//! these bodies. The portable behavior of the classes is `tests/box_regressions/atomics.kt`; this
//! test pins what the shared KLIB lowering makes of the Native bodies: one atomic operation on that
//! field, or on an element of that array, per member.

use krusty::backend::{Artifact, Backend, CheckedIrFile, CheckedIrUnitKind};
use krusty::diag::DiagSink;
use krusty::ir::{IrCheckedOperation, IrExpr};
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};

const CONSUMER: &str = "\
@file:OptIn(kotlin.concurrent.atomics.ExperimentalAtomicApi::class)

import kotlin.concurrent.atomics.*

fun ints(a: AtomicInt): Int {
    a.store(1)
    a.exchange(2)
    a.compareAndSet(2, 3)
    a.compareAndExchange(3, 4)
    a.fetchAndAdd(5)
    if (a.toString() != a.load().toString()) return -1
    return a.load()
}

fun longs(a: AtomicLong): Long {
    a.store(1L)
    a.exchange(2L)
    a.compareAndSet(2L, 3L)
    a.compareAndExchange(3L, 4L)
    a.fetchAndAdd(5L)
    return a.load()
}

fun booleans(a: AtomicBoolean): Boolean {
    a.store(true)
    a.exchange(false)
    a.compareAndSet(false, true)
    a.compareAndExchange(true, false)
    return a.load()
}

fun references(a: AtomicReference<String>): String {
    a.store(\"y\")
    a.exchange(\"z\")
    a.compareAndSet(\"z\", \"w\")
    a.compareAndExchange(\"w\", \"v\")
    if (a.toString() != a.load()) return \"toString\"
    return a.load()
}

fun box(): String {
    references(AtomicReference(\"x\"))
    ints(AtomicInt(0))
    longs(AtomicLong(0L))
    booleans(AtomicBoolean(false))
    return \"OK\"
}
";

/// Every atomic field operation the dependency support units lower, as
/// `(class, field, operation)` in the order the units hold them.
struct AtomicFieldProbe {
    native: krusty_native::CraneliftBackend,
    seen: std::cell::RefCell<Vec<(String, String, String)>>,
    /// Each unit function with a dispatch receiver, as `(class, function, a method of its class)`.
    members: std::cell::RefCell<Vec<(String, String, bool)>>,
    /// Every atomic array element operation, as `(array class, operation)`.
    elements: std::cell::RefCell<Vec<(String, String)>>,
}

impl Backend for AtomicFieldProbe {
    type State = ();

    fn compilation_target(&self) -> krusty::compilation_target::CompilationTarget {
        self.native.compilation_target()
    }

    fn emits_dependency_support(&self) -> bool {
        self.native.emits_dependency_support()
    }

    fn common_ir_preparation(&self) -> Option<krusty::backend::CommonIrPreparation> {
        self.native.common_ir_preparation()
    }

    fn represents_dependency_classifier(
        &self,
        classifier: krusty::backend::DependencyClassifier,
    ) -> bool {
        self.native.represents_dependency_classifier(classifier)
    }

    fn numbers_slots(&self) -> bool {
        self.native.numbers_slots()
    }

    fn realizes_bodyless_dependency_support(&self) -> bool {
        self.native.realizes_bodyless_dependency_support()
    }

    fn realizes_dependency_intrinsic(
        &self,
        operation: krusty::backend::BackendCompilerIntrinsic,
    ) -> bool {
        self.native.realizes_dependency_intrinsic(operation)
    }

    fn realizes_dependency_getter(
        &self,
        signature: krusty::backend::DependencyGetterSignature<'_>,
    ) -> bool {
        self.native.realizes_dependency_getter(signature)
    }

    fn same_module_inlining(&self) -> krusty::ir::SameModuleInlining {
        self.native.same_module_inlining()
    }

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        _state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        if file.kind == CheckedIrUnitKind::DependencySupport {
            for (id, function) in file.ir.functions.iter().enumerate() {
                let Some(owner) = function.dispatch_receiver else {
                    continue;
                };
                let Some(class) = file.ir.class_id_by_name(owner) else {
                    continue;
                };
                let id = krusty::ir::FunId::try_from(id).expect("a function id fits");
                self.members.borrow_mut().push((
                    owner.render().replace('/', "."),
                    function.name.clone(),
                    file.ir.classes[class as usize].methods.contains(&id),
                ));
            }
        }
        for expression in &file.ir.exprs {
            let IrExpr::Checked(checked) = expression else {
                continue;
            };
            if let IrCheckedOperation::AtomicArrayElement {
                operation, array, ..
            } = &**checked
            {
                assert_eq!(file.kind, CheckedIrUnitKind::DependencySupport);
                let array = file
                    .ir
                    .checked_type(*array)
                    .expect("an atomic array element access names a typed array");
                let class = array
                    .obj_internal()
                    .expect("an atomic array element access names an array class");
                self.elements
                    .borrow_mut()
                    .push((class.render().replace('/', "."), format!("{operation:?}")));
                continue;
            }
            let IrCheckedOperation::AtomicField {
                operation,
                class,
                index,
                ..
            } = &**checked
            else {
                continue;
            };
            assert_eq!(file.kind, CheckedIrUnitKind::DependencySupport);
            let class = &file.ir.classes[*class as usize];
            self.seen.borrow_mut().push((
                class.fq_name.render().replace('/', "."),
                class.fields[*index as usize].name.clone(),
                format!("{operation:?}"),
            ));
        }
        Vec::new()
    }

    fn finalize(&self, _state: Self::State, _module_name: &str) -> Vec<Artifact> {
        Vec::new()
    }
}

/// The probe after the Native stdlib lowered `consumer`'s dependency support, or `None` without
/// a Native distribution for the host.
fn lowered(consumer: &str) -> Option<AtomicFieldProbe> {
    let Some(root) = std::env::var_os("KRUSTY_KOTLIN_NATIVE").map(std::path::PathBuf::from) else {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return None;
    };
    let target = krusty_native::NativeTarget::host()?;
    let libraries = KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
        root.join("klib/common/stdlib"),
        KlibStdlibSemantics::Native,
    )])
    .expect("open the Native stdlib KLIB");
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &[krusty::source::SourceInput::kotlin(consumer).with_file_stem("Consumer")],
        krusty::frontend::PlatformProvider::new(
            krusty::compilation_target::CompilationTarget::Native,
            Box::new(libraries.for_compilation()),
        ),
        &krusty::features::LangFeatures::from_environment(),
        &mut diags,
    );
    let probe = AtomicFieldProbe {
        native: krusty_native::CraneliftBackend::new(target).with_entry(krusty_native::Entry::Box),
        seen: std::cell::RefCell::default(),
        members: std::cell::RefCell::default(),
        elements: std::cell::RefCell::default(),
    };
    krusty::compiler::emit_analyzed(
        analysis,
        &["Consumer".to_owned()],
        &probe,
        "consumer",
        &mut diags,
    );
    assert_eq!(
        diags
            .diags
            .iter()
            .map(|diag| diag.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    Some(probe)
}

#[test]
fn native_atomic_members_lower_to_one_atomic_operation_on_their_field() {
    let Some(probe) = lowered(CONSUMER) else {
        return;
    };
    let field = |class: &str, operation: &str| {
        (
            format!("kotlin.concurrent.atomics.{class}"),
            "value".to_owned(),
            operation.to_owned(),
        )
    };
    let mut expected = Vec::new();
    for class in ["AtomicInt", "AtomicLong"] {
        for operation in [
            "Load",
            "FetchAndAdd",
            "CompareAndExchange",
            "CompareAndSet",
            "Exchange",
            "Store",
        ] {
            expected.push(field(class, operation));
        }
    }
    // `AtomicReference<T>` is generic; it is final, so its members are emitted with it as the
    // non-generic classes' are.
    for class in ["AtomicBoolean", "AtomicReference"] {
        for operation in [
            "Load",
            "CompareAndExchange",
            "CompareAndSet",
            "Exchange",
            "Store",
        ] {
            expected.push(field(class, operation));
        }
    }
    assert_eq!(probe.seen.into_inner(), expected);
    // `toString` reads the field through its private getter, which no class table numbers. It
    // is still a method of its class, which a call of it runs directly.
    let members = probe.members.into_inner();
    let strays = members
        .iter()
        .filter(|(_, _, belongs)| !belongs)
        .collect::<Vec<_>>();
    assert_eq!(strays, Vec::<&(String, String, bool)>::new());
    assert!(members.contains(&(
        "kotlin.concurrent.atomics.AtomicInt".to_owned(),
        "<get-value>".to_owned(),
        true
    )));
}

const ARRAY_CONSUMER: &str = "\
@file:OptIn(kotlin.concurrent.atomics.ExperimentalAtomicApi::class)

import kotlin.concurrent.atomics.*

fun ints(a: AtomicIntArray): Int {
    a.storeAt(0, 1)
    a.exchangeAt(0, 2)
    a.compareAndSetAt(0, 2, 3)
    a.compareAndExchangeAt(0, 3, 4)
    a.fetchAndAddAt(0, 5)
    return a.loadAt(0) + a.size
}

fun longs(a: AtomicLongArray): Long {
    a.storeAt(0, 1L)
    a.exchangeAt(0, 2L)
    a.compareAndSetAt(0, 2L, 3L)
    a.compareAndExchangeAt(0, 3L, 4L)
    a.fetchAndAddAt(0, 5L)
    return a.loadAt(0)
}

fun references(a: AtomicArray<String>): String {
    a.storeAt(0, \"y\")
    a.exchangeAt(0, \"z\")
    a.compareAndSetAt(0, \"z\", \"w\")
    a.compareAndExchangeAt(0, \"w\", \"v\")
    return a.loadAt(0)
}

fun box(): String {
    ints(AtomicIntArray(intArrayOf(0)))
    longs(AtomicLongArray(1))
    references(AtomicArray(arrayOf(\"x\")))
    return \"OK\"
}
";

#[test]
fn native_atomic_array_members_lower_to_one_atomic_operation_on_an_element() {
    let Some(probe) = lowered(ARRAY_CONSUMER) else {
        return;
    };
    assert_eq!(
        probe.seen.into_inner(),
        Vec::<(String, String, String)>::new()
    );
    // `toString` loads each element as `loadAt` does; `AtomicArray<T>` has no addition.
    let mut expected = Vec::new();
    for (array, operations) in [
        ("kotlin.IntArray", &["Load", "Load", "FetchAndAdd"][..]),
        ("kotlin.LongArray", &["Load", "Load", "FetchAndAdd"][..]),
        ("kotlin.Array", &["Load", "Load"][..]),
    ] {
        for operation in
            operations
                .iter()
                .chain(&["CompareAndExchange", "CompareAndSet", "Exchange", "Store"])
        {
            expected.push((array.to_owned(), (*operation).to_owned()));
        }
    }
    assert_eq!(probe.elements.into_inner(), expected);
}
