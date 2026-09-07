//! The JVM [`Backend`]: lowers each already-checked file to `.class` files (with `@Metadata` inside
//! the class bytes) and emits the `META-INF/<module>.kotlin_module` package → facade mapping.

use crate::backend::{
    Artifact, Backend, BackendClassifierSource, BackendModuleFacts, CheckedBackendClassifiers,
};
use crate::diag::DiagSink;
use crate::jvm::names::{file_class_name, type_descriptor};
use crate::types::Ty;

/// Why the JVM realization pipeline declined a file: the named pass met a shape it can't lower yet, so the
/// caller must skip (or diagnose) the file rather than miscompile it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `lower_value_classes` — a `@JvmInline value class` shape not yet supported.
    ValueClasses,
    /// `lower_suspend` — a `suspend fun` shape not yet supported.
    Suspend,
    /// `derive_bridges` — an override whose bridge cannot be modeled (a bounded type-param erasure, or a
    /// `suspend` override the coroutine pass would rewrite out from under the bridge).
    Bridges,
    /// JVM default-argument operands could not be realized from a checked semantic call.
    DefaultCalls,
}

/// THE post-lowering, pre-emit JVM pass pipeline — the single definition the streaming backend
/// calls, so a newly
/// added pass lands in all of them by construction. Hand-replicating this sequence has twice
/// produced false-green test runs (a pass added here but missed in a replica → IllegalAccessError
/// miscompiles the gate never saw); a unit test below bans direct calls to the individual passes.
///
/// Runs, in order:
/// 1. `plugins::run_enabled` — compiler-extension plugins (kotlinx.serialization) synthesize
///    declarations from the file's annotations; no-op without a trigger annotation.
///
/// 2. `realize_top_level_jvm_fields` — select public field storage for eligible top-level
///    `@JvmField` declarations through stable property/layout identities.
///
/// 3. `lower_companion_properties` — realize supported companion backing fields as JVM outer statics.
///    Common IR keeps the ordinary declaration and semantic initializer for other targets.
///
/// 4. `elide_default_property_stores` — omit declaration stores already supplied by JVM field
///    initialization. Common IR retains them for targets without zero-initialized fields.
///
/// 5. `derive_bridges` — synthesize the `ACC_BRIDGE` methods an override needs to be reachable through
///    a supertype's erased descriptor. A bridge is a JVM realization of an override, not a Kotlin
///    declaration, so lowering records only the declarations and this pass derives the bridges.
///
/// 6. `apply_collection_bridge_barriers` — attach JVM collection bridge semantics.
///
/// 7. `lower_value_classes` — realize `@JvmInline value class`es as their unboxed underlying type
///    (the IR keeps them as plain classes so JS / a native-value-type JVM are unaffected).
///
/// 8. `realize_default_calls` — materialize JVM placeholders, masks, and marker operands only after
///    value-class lowering has fixed their physical carriers.
///
/// 9. `lower_class_capture_slots` — realize marked mutable class captures as JVM `Ref` holders.
///
/// 10. `lower_suspend` — realize `suspend fun`s as their continuation-passing-style ABI.
///
/// 11. `mark_must_inline_lambdas` — drop the dead standalone impl of a must-inline call's
///     (`require`/`check`) message lambda; it is spliced at the call site.
///
/// 12. `reparent_lambda_impls` — a lambda impl method must be a member of the CLASS whose code emits
///     its `invokedynamic` (the impl is PRIVATE, kotlinc's placement, so a cross-class handle would
///     be an IllegalAccessError). Lowering attaches impls per `cur_class`, which misses code that
///     ends up in a class only later: enum-entry constructor arguments and suspend-lambda state
///     machines. Runs after all IR→IR transforms, before emit.
///
/// Per-site concerns (timing counters, bail-reason strings, diagnostics) stay at the call sites.
/// Streaming JVM pipeline entry. Native plugins consume only checked common-IR declaration
/// metadata; the reparsed Pass-2 source unit is not part of their contract.
pub fn run_backend_passes_with_checked_metadata(
    ir: &mut crate::ir::IrFile,
    facade: &str,
    module_name: &str,
    classifiers: &CheckedBackendClassifiers<'_>,
    classpath: &crate::jvm::classpath::Classpath,
    continuation_metadata: &mut crate::jvm::suspend::ContinuationMetadataMap,
    stems: &[String],
) -> Result<(), SkipReason> {
    crate::plugins::run_enabled_from_ir(ir, module_name, jvm_plugin_type_descriptor);
    run_backend_passes_after_plugins(
        ir,
        facade,
        classifiers.module().source_value_classes(),
        classifiers.module().metadata_readable_value_classes(),
        classpath,
        continuation_metadata,
        Some(stems),
    )
}

fn run_backend_passes_after_plugins(
    ir: &mut crate::ir::IrFile,
    facade: &str,
    module_value_classes: &std::collections::HashMap<crate::types::TypeName, Ty>,
    module_readable_value_classes: &std::collections::HashSet<crate::types::TypeName>,
    classpath: &crate::jvm::classpath::Classpath,
    continuation_metadata: &mut crate::jvm::suspend::ContinuationMetadataMap,
    stems: Option<&[String]>,
) -> Result<(), SkipReason> {
    crate::jvm::annotation_constructions::lower_annotation_constructions(ir, facade);
    // A property's own annotations become a synthetic marker method — a JVM realization of a Kotlin
    // declaration that has no class-file form. Before the value-class pass, which renames a marker
    // together with its mangled getter.
    crate::jvm::property_annotations::synthesize_property_annotation_markers(ir);
    // A package property remains a semantic declaration plus a checked common-IR layout. Join the
    // stable identities here, where `@JvmField` becomes a JVM storage decision.
    crate::jvm::property_storage::realize_top_level_jvm_fields(ir);
    // Companion backing-field hoisting is a JVM storage choice. Common IR retains the ordinary
    // property declaration and semantic initializer; this pass selects the outer-static realization.
    crate::jvm::companion::lower_companion_properties(ir);
    // The JVM supplies default field values before any constructor runs. Elide only source
    // declaration stores recorded by exact ExprId; common IR and other targets keep them.
    crate::jvm::property_storage::elide_default_property_stores(ir);
    // Kotlin parameter nullability is already fixed in common IR. Select the JVM's entry-guard
    // realization before generic/value-class erasure changes the physical parameter types; those
    // later representation passes may then remove a guard whose carrier becomes primitive.
    crate::jvm::parameter_assertions::realize(ir);
    // Common IR retains source type-parameter identities and complete intersections. Select the JVM
    // class-bound erasure here, once, before any descriptor-sensitive backend pass runs.
    crate::jvm::reified_operations::realize(ir);
    crate::jvm::generic_erasure::lower_function_type_parameters(ir);
    // Bridges are a JVM realization of an override, derived here from the IR's own declarations and the
    // checker's supertype view. Runs BEFORE the barrier pass (which annotates existing bridges) and
    // before the value-class pass (which retargets them once mangled names are known).
    crate::jvm::bridges::derive_bridges(ir, classpath)?;
    apply_collection_bridge_barriers(ir);
    // Same-module SOURCE value classes (internal name → sole-field underlying) for the value-class pass's
    // erasure/mangle map — a value class declared in ANOTHER file of this module. Read from the frontend
    // symbols directly, NOT surfaced through the resolver's library view (which would change the checker's
    // construction/member resolution for source value classes).
    crate::jvm::value_classes::apply_override_final_drop(ir);
    if !crate::jvm::value_classes::lower_value_classes(
        ir,
        classpath,
        module_value_classes,
        module_readable_value_classes,
    ) {
        return Err(SkipReason::ValueClasses);
    }
    if let Some(stems) = stems {
        crate::jvm::module_calls::realize_default_calls(ir, stems)
            .map_err(|_| SkipReason::DefaultCalls)?;
    }
    crate::jvm::shared_captures::lower_class_capture_slots(ir);
    if !crate::jvm::suspend::lower_suspend(ir, facade, continuation_metadata) {
        return Err(SkipReason::Suspend);
    }
    crate::jvm::ir_emit::realize_lambda_impl_names(ir);
    crate::jvm::ir_emit::mark_must_inline_lambdas(ir);
    crate::jvm::ir_emit::reparent_lambda_impls(ir);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BridgeBarrierOutcome {
    False,
    NotFound,
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BridgeBarrier {
    pub parameter: usize,
    pub outcome: BridgeBarrierOutcome,
}

#[derive(Clone, Copy, Debug)]
enum CollectionOwner {
    Collection,
    MutableCollection,
    List,
    Map,
}

impl CollectionOwner {
    fn matches(self, owner: crate::types::TypeName) -> bool {
        let names: &[&str] = match self {
            CollectionOwner::Collection => &[
                "java/util/Collection",
                "java/util/List",
                "java/util/Set",
                "kotlin/collections/Collection",
                "kotlin/collections/MutableCollection",
                "kotlin/collections/List",
                "kotlin/collections/MutableList",
                "kotlin/collections/Set",
                "kotlin/collections/MutableSet",
            ],
            CollectionOwner::MutableCollection => &[
                "java/util/Collection",
                "java/util/List",
                "java/util/Set",
                "kotlin/collections/MutableCollection",
                "kotlin/collections/MutableList",
                "kotlin/collections/MutableSet",
            ],
            CollectionOwner::List => &[
                "java/util/List",
                "kotlin/collections/List",
                "kotlin/collections/MutableList",
            ],
            CollectionOwner::Map => &[
                "java/util/Map",
                "kotlin/collections/Map",
                "kotlin/collections/MutableMap",
            ],
        };
        names.iter().any(|name| owner.matches(name))
    }
}

fn collection_bridge_semantics(
    bridge: &crate::ir::Bridge,
) -> Option<(CollectionOwner, BridgeBarrier)> {
    let (owner, outcome) = match bridge.name.as_str() {
        "contains"
            if bridge.erased_ret == crate::types::Ty::Boolean
                && bridge.concrete_ret == crate::types::Ty::Boolean =>
        {
            (CollectionOwner::Collection, BridgeBarrierOutcome::False)
        }
        "remove"
            if bridge.erased_ret == crate::types::Ty::Boolean
                && bridge.concrete_ret == crate::types::Ty::Boolean =>
        {
            (
                CollectionOwner::MutableCollection,
                BridgeBarrierOutcome::False,
            )
        }
        "indexOf" | "lastIndexOf"
            if bridge.erased_ret == crate::types::Ty::Int
                && bridge.concrete_ret == crate::types::Ty::Int =>
        {
            (CollectionOwner::List, BridgeBarrierOutcome::NotFound)
        }
        "containsKey"
            if bridge.erased_ret == crate::types::Ty::Boolean
                && bridge.concrete_ret == crate::types::Ty::Boolean =>
        {
            (CollectionOwner::Map, BridgeBarrierOutcome::False)
        }
        "get" if bridge.erased_ret.is_reference() && bridge.concrete_ret.is_reference() => {
            (CollectionOwner::Map, BridgeBarrierOutcome::Null)
        }
        _ => return None,
    };
    let parameter = 0;
    (bridge.erased_params.len() == 1
        && bridge.concrete_params.len() == 1
        && bridge.erased_params[parameter].is_erased_top()
        && bridge.concrete_params[parameter].is_reference()
        && !bridge.concrete_params[parameter].is_erased_top())
    .then_some((parameter, outcome))
    .map(|(parameter, outcome)| (owner, BridgeBarrier { parameter, outcome }))
}

pub(crate) fn bridge_barrier(bridge: &crate::ir::Bridge) -> Option<BridgeBarrier> {
    bridge
        .type_safe_barrier
        .then(|| collection_bridge_semantics(bridge))
        .flatten()
        .map(|(_, barrier)| barrier)
}

fn apply_collection_bridge_barriers(ir: &mut crate::ir::IrFile) {
    for class in &mut ir.classes {
        let owners = ir
            .classifier_hierarchies
            .get(&class.fq_name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for bridge in &mut class.bridges {
            let semantics = collection_bridge_semantics(bridge);
            bridge.type_safe_barrier = semantics.is_some_and(|(required, _)| {
                owners
                    .iter()
                    .any(|entry| required.matches(entry.classifier))
            });
            crate::trace_compiler!(
                "lower",
                "collection bridge class={} name={} hierarchy={:?} semantics={:?} barrier={}",
                class.fq_name,
                bridge.name,
                owners
                    .iter()
                    .map(|entry| entry.classifier)
                    .collect::<Vec<_>>(),
                semantics,
                bridge.type_safe_barrier,
            );
        }
    }
}

fn jvm_plugin_type_descriptor(ty: Ty) -> Option<String> {
    Some(type_descriptor(ty))
}

/// The JVM backend holds the shared classpath (`Rc`, same instance as `JvmLibraries`) so the emitter
/// can read inline-function bodies for the bytecode inliner.
pub struct JvmBackend {
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
    /// Class-file major version to emit (`-jvm-target`), or `None` for krusty's default (v52).
    class_major: Option<u16>,
    jvm_default: crate::jvm::ir_emit::JvmDefaultMode,
    lambda_modes: crate::jvm::ir_emit::LambdaModes,
    /// Whether to emit the `Intrinsics.checkNotNullParameter` guards (`-Xno-param-assertions`
    /// clears this).
    param_assertions: bool,
    /// Whether to emit the `Intrinsics.checkNotNullExpressionValue` guard on a narrowed platform
    /// value (`-Xno-call-assertions` clears this).
    call_assertions: bool,
}

impl JvmBackend {
    pub fn new(cp: std::rc::Rc<crate::jvm::classpath::Classpath>) -> JvmBackend {
        JvmBackend {
            cp,
            class_major: None,
            jvm_default: crate::jvm::ir_emit::JvmDefaultMode::default(),
            lambda_modes: crate::jvm::ir_emit::LambdaModes::default(),
            param_assertions: true,
            call_assertions: true,
        }
    }

    /// `-Xno-param-assertions` passes `false`: emit no `Intrinsics.checkNotNullParameter` guards.
    pub fn with_param_assertions(mut self, enabled: bool) -> JvmBackend {
        self.param_assertions = enabled;
        self
    }

    /// `-Xno-call-assertions` passes `false`: emit no `Intrinsics.checkNotNullExpressionValue` guard
    /// where a platform value is narrowed to a declared non-null type.
    pub fn with_call_assertions(mut self, enabled: bool) -> JvmBackend {
        self.call_assertions = enabled;
        self
    }

    /// `-jvm-default`: which JVM shape an interface's members with bodies are compiled into.
    pub fn with_jvm_default(mut self, mode: crate::jvm::ir_emit::JvmDefaultMode) -> JvmBackend {
        self.jvm_default = mode;
        self
    }

    /// Independently select `-Xlambdas` and `-Xsam-conversions` realization strategies.
    pub fn with_lambda_modes(mut self, modes: crate::jvm::ir_emit::LambdaModes) -> JvmBackend {
        self.lambda_modes = modes;
        self
    }

    /// Set the class-file version subsequent emits target (from the CLI's `-jvm-target`).
    pub fn with_class_major(mut self, major: Option<u16>) -> JvmBackend {
        self.class_major = major;
        self
    }
}

/// The per-file emit configuration krusty SHIPS with — ONE definition, so an in-process caller (the
/// test harness, an embedder) emits exactly what `krusty -d …` does. It carries the class version
/// (`-jvm-target`), the `SourceFile` name (the origin `.kt`; kotlinc uses the simple name,
/// reconstructed from the stem — directories are already stripped), the `-module-name`, and the
/// per-class `@Metadata` switch. Threaded explicitly into emission so every class (incl. synthetics)
/// inherits it. `EmitOptions::default()` is NOT the pre-class-metadata shape — it emits per-class
/// `@Metadata` too; what it lacks is the `SourceFile`, the inner-class resolver and the `-jvm-target`
/// class version, which is why a caller claiming to emit shipping bytes must start here.
/// `KRUSTY_NO_CLASS_METADATA` (read below) is how a shipping caller gets facade-only output back; it is
/// consulted ONLY here, so a caller holding some other `EmitOptions` opts out by setting
/// `emit_class_metadata: false` itself.
pub fn shipping_emit_options(
    stem: &str,
    module_name: &str,
    class_major: Option<u16>,
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::ir_emit::EmitOptions {
    // Module/conformance inputs may retain a source-relative prefix (`helpers/Foo`) even though the
    // CLI has already reduced its input to `Foo`. `SourceFile` is a simple filename on every JVM
    // class, so normalize at this shared boundary instead of making each non-CLI caller grow its own
    // path branch. Accept both separators because Kotlin testdata names are logical source paths and
    // are not guaranteed to use the host platform's separator.
    let source_stem = stem.rsplit(['/', '\\']).next().unwrap_or(stem);
    crate::jvm::ir_emit::EmitOptions {
        class_major,
        source_file: Some(format!("{source_stem}.kt")),
        // kotlinc records `classModuleName` in @Metadata unless the module is the default `main`.
        module_name: (module_name != "main").then(|| module_name.to_string()),
        // Per-invocation strategies; the CLI overrides them on the backend.
        lambda_modes: crate::jvm::ir_emit::LambdaModes::default(),
        // Compute + emit each class's own `@Metadata`. Without it a krusty-compiled CLASS is
        // unreadable BY KRUSTY: the facade metadata describes top-level declarations only, so a
        // second compilation sees no constructor/member parameter names (named arguments) and no
        // `operator` marks (destructuring). A shape `build_class_metadata` has not verified against
        // kotlinc declines individually and emits nothing, so this cannot write an unverified
        // payload. `KRUSTY_NO_CLASS_METADATA` restores the facade-only output for bisecting.
        emit_class_metadata: std::env::var_os("KRUSTY_NO_CLASS_METADATA").is_none(),
        jvm_default: crate::jvm::ir_emit::JvmDefaultMode::default(),
        // `-Xno-param-assertions` is applied by the caller (`with_param_assertions`); the shipping
        // default emits the guards, as kotlinc does.
        param_assertions: true,
        inner_class_resolver: Some(classpath_inner_class_resolver(cp)),
    }
}

fn checked_module_inner_class_resolver(
    module: &BackendModuleFacts,
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::classfile::InnerClassResolver {
    module_inner_class_resolver_from_shapes(
        module
            .classifiers()
            .map(|(classifier, shape)| InnerModuleClassifier {
                classifier,
                visibility: shape.access.visibility(),
                inner: shape.outer_instance.is_some(),
                annotation: shape.is_annotation(),
                interface: shape.is_interface(),
                enum_class: shape.is_enum(),
                abstract_class: shape.is_abstract,
                final_class: !shape.is_abstract && !shape.is_extensible,
            }),
        cp,
    )
}

#[derive(Clone, Copy)]
struct InnerModuleClassifier {
    classifier: crate::types::TypeName,
    visibility: crate::types::Visibility,
    inner: bool,
    annotation: bool,
    interface: bool,
    enum_class: bool,
    abstract_class: bool,
    final_class: bool,
}

fn module_inner_class_resolver_from_shapes(
    classes: impl IntoIterator<Item = InnerModuleClassifier>,
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::classfile::InnerClassResolver {
    const PUBLIC: u16 = 0x0001;
    const PROTECTED: u16 = 0x0004;
    const PRIVATE: u16 = 0x0002;
    const STATIC: u16 = 0x0008;
    const FINAL: u16 = 0x0010;
    const INTERFACE: u16 = 0x0200;
    const ABSTRACT: u16 = 0x0400;
    const ANNOTATION: u16 = 0x2000;
    const ENUM: u16 = 0x4000;

    let classes = classes.into_iter().collect::<Vec<_>>();
    let module_names = classes
        .iter()
        .map(|class| class.classifier)
        .collect::<std::collections::HashSet<_>>();
    let mut source = std::collections::HashMap::new();
    for class in classes {
        let internal = class.classifier.render();
        // Only MEMBER-nested classes get snapshot entries, and the outer boundary is NOT the last
        // `$` (mirroring `register_inner_classes`): the boundary is the longest proper prefix that
        // is itself a module class. A name whose remainder still carries `$` past that boundary is
        // a hoisted LOCAL class (`pkg/Outer$m$Local` — `m` is a function, not a class) or a
        // backticked simple name; kotlinc spells a local's entry with `outer_class_info_index = 0`,
        // and inventing an outer makes the loader chase a class that does not exist
        // (`NoClassDefFoundError`). The two are not distinguishable from the name alone (a
        // backticked MEMBER `` class `X$Y` `` is denotable cross-file and would deserve an entry),
        // so the snapshot omits the ambiguous shape rather than risk fabricating an outer.
        let Some(boundary) = internal
            .char_indices()
            .filter(|&(_, ch)| ch == '$')
            .map(|(at, _)| at)
            .filter(|&at| module_names.contains(&crate::types::type_name(&internal[..at])))
            .max()
        else {
            continue; // top-level (or nested under nothing this module declares)
        };
        let (outer, simple) = (&internal[..boundary], &internal[boundary + 1..]);
        if simple.contains('$') {
            continue;
        }
        let visibility = match class.visibility {
            crate::types::Visibility::Protected => PROTECTED,
            crate::types::Visibility::Private => PRIVATE,
            _ => PUBLIC,
        };
        let mut access = visibility | if class.inner { 0 } else { STATIC };
        if class.annotation {
            access |= INTERFACE | ABSTRACT | ANNOTATION;
        } else if class.interface {
            access |= INTERFACE | ABSTRACT;
        } else if class.enum_class {
            access |= FINAL | ENUM;
        } else if class.abstract_class {
            access |= ABSTRACT;
        } else if class.final_class {
            access |= FINAL;
        }
        source.insert(
            internal.clone(),
            crate::jvm::classfile::InnerClassDetails {
                outer: Some(outer.to_string()),
                name: Some(simple.to_string()),
                access,
            },
        );
    }
    let classpath = classpath_inner_class_resolver(cp);
    std::rc::Rc::new(move |internal: &str| {
        source
            .get(internal)
            .cloned()
            .or_else(|| classpath(internal))
    })
}

pub fn classpath_inner_class_resolver(
    cp: std::rc::Rc<crate::jvm::classpath::Classpath>,
) -> crate::jvm::classfile::InnerClassResolver {
    std::rc::Rc::new(move |internal: &str| {
        // The class file first — it is authoritative whenever the nested class has one. A mapped
        // builtin whose JVM class is absent (no JDK on the classpath) still declares the same nesting
        // in its `.kotlin_builtins` entry; without this fallback the reference emits no `InnerClasses`
        // attribute at all, so the JDK-less class file diverges from the JDK-present one.
        cp.find(internal)
            .and_then(|class| {
                let entry = class.inner_class_self()?;
                Some(crate::jvm::classfile::InnerClassDetails {
                    outer: entry.outer.clone(),
                    name: entry.name.clone(),
                    access: entry.access,
                })
            })
            .or_else(|| {
                let (outer, name, access) = cp.builtin_nested_class(internal)?;
                Some(crate::jvm::classfile::InnerClassDetails {
                    outer: Some(outer),
                    name: Some(name),
                    access,
                })
            })
    })
}

/// package → file-facade class names, accumulated across files for the `.kotlin_module` mapping.
#[derive(Default)]
pub struct JvmState {
    module_packages: std::collections::BTreeMap<String, Vec<String>>,
}

impl JvmBackend {
    fn emit_streamed_ir(
        &self,
        file: crate::backend::CheckedIrFile<'_>,
        property_realizations: crate::jvm::property_realizations::PropertyRealizations,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let crate::backend::CheckedIrFile {
            mut ir,
            source,
            classifiers,
            module_name,
            stems,
        } = file;
        let stem = &stems[source.raw() as usize];
        let package = ir.package.clone().unwrap_or_default();
        let facade_name = file_class_name(stem, ir.package.as_deref());
        let mut continuation_metadata = crate::jvm::suspend::ContinuationMetadataMap::default();
        if let Err(reason) = run_backend_passes_with_checked_metadata(
            &mut ir,
            &facade_name,
            module_name,
            &classifiers,
            &self.cp,
            &mut continuation_metadata,
            stems,
        ) {
            report_backend_pass_failure(reason, diags);
            return Vec::new();
        }
        if let Err(message) = crate::jvm::declaration_collisions::validate(&ir) {
            diags.error(crate::diag::Span::new(0, 0), message);
            return Vec::new();
        }
        let metadata = facade_package_metadata_from_ir(&ir, module_name);
        let has_facade_members = metadata.is_some();
        let inner_class_resolver =
            checked_module_inner_class_resolver(classifiers.module(), self.cp.clone());
        self.emit_backend_ready_ir(
            ir,
            stem,
            module_name,
            facade_name,
            package,
            &classifiers,
            inner_class_resolver,
            continuation_metadata,
            metadata,
            has_facade_members,
            property_realizations,
            state,
            diags,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_backend_ready_ir(
        &self,
        mut ir: crate::ir::IrFile,
        stem: &str,
        module_name: &str,
        facade_name: String,
        package: String,
        classifiers: &dyn BackendClassifierSource,
        inner_class_resolver: crate::jvm::classfile::InnerClassResolver,
        continuation_metadata: crate::jvm::suspend::ContinuationMetadataMap,
        metadata: Option<crate::jvm::ir_emit::KotlinMetadata>,
        has_facade_members: bool,
        property_realizations: crate::jvm::property_realizations::PropertyRealizations,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let mut outputs = Vec::new();
        if !self.param_assertions {
            crate::jvm::ir_emit::strip_param_assertions(&mut ir);
        }
        if !self.call_assertions {
            crate::jvm::ir_emit::strip_call_assertions(&mut ir);
        }
        let mut emit_opts =
            shipping_emit_options(stem, module_name, self.class_major, self.cp.clone())
                .with_jvm_default(self.jvm_default)
                .with_lambda_modes(self.lambda_modes)
                .with_param_assertions(self.param_assertions);
        emit_opts.inner_class_resolver = Some(inner_class_resolver);
        let run = crate::jvm::ir_emit::EmitRun::default();
        let emit_metadata = crate::jvm::ir_emit::EmitMetadata {
            facade: metadata.as_ref(),
            continuations: &continuation_metadata,
        };
        let classes = crate::jvm::ir_emit::emit_all_with_checked_classifiers(
            &ir,
            &facade_name,
            &*self.cp,
            emit_metadata,
            &emit_opts,
            &run,
            classifiers,
            &property_realizations,
        );
        let Some(classes) = classes else {
            if let Some(reason) = run.inline_bail() {
                diags.error(
                    crate::diag::Span::new(0, 0),
                    format!("krusty: JVM backend inline error: {reason}"),
                );
                return outputs;
            }
            if let Some(reason) = run.emit_error() {
                diags.error(crate::diag::Span::new(0, 0), reason);
                return outputs;
            }
            diags.error(
                crate::diag::Span::new(0, 0),
                "krusty: this construct is not yet supported by the IR backend".to_string(),
            );
            return outputs;
        };
        for (internal, bytes) in classes {
            outputs.push((format!("{internal}.class"), bytes));
        }

        if has_facade_members {
            let facade = facade_name
                .rsplit('/')
                .next()
                .unwrap_or(&facade_name)
                .to_string();
            state
                .module_packages
                .entry(package)
                .or_default()
                .push(facade);
        }
        outputs
    }
}

fn report_backend_pass_failure(reason: SkipReason, diags: &mut DiagSink) {
    if reason == SkipReason::DefaultCalls {
        diags.error(
            crate::diag::Span::new(0, 0),
            "internal error: invalid checked default-argument realization".to_string(),
        );
        return;
    }
    let what = match reason {
        SkipReason::ValueClasses => "value-class",
        SkipReason::Suspend => "suspend-function",
        SkipReason::Bridges => "bridge-method",
        SkipReason::DefaultCalls => unreachable!(),
    };
    diags.error(
        crate::diag::Span::new(0, 0),
        format!("krusty: this {what} shape is not yet supported by the IR backend"),
    );
}

impl Backend for JvmBackend {
    type State = JvmState;

    fn lower_ir_file(
        &self,
        mut file: crate::backend::CheckedIrFile<'_>,
        state: &mut JvmState,
        diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let stem = &file.stems[file.source.raw() as usize];
        let facade = file_class_name(stem, file.ir.package.as_deref());
        if let Err(error) = crate::jvm::ranges::realize(&mut file.ir, self.cp.clone()) {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: cannot realize checked range operation: {error:?}"),
            );
            return Vec::new();
        }
        if let Err(target) =
            crate::jvm::function_references::realize(&mut file.ir, &self.cp, &facade)
        {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!(
                    "internal error: missing JVM function-reference realization for {target:?}"
                ),
            );
            return Vec::new();
        }
        if let Err(target) =
            crate::jvm::property_references::realize(&mut file.ir, file.stems, &self.cp, &facade)
        {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!(
                    "internal error: missing JVM property-reference realization for {target:?}"
                ),
            );
            return Vec::new();
        }
        // A checked annotation constructor names the semantic annotation declaration. The JVM
        // realizes it as a generated concrete implementation before ordinary dependency
        // constructors are assigned physical descriptors/default stubs.
        crate::jvm::annotation_constructions::lower_annotation_constructions(&mut file.ir, &facade);
        if let Err(target) = crate::jvm::external_calls::realize(&mut file.ir, &self.cp) {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: missing JVM dependency realization for {target}"),
            );
            return Vec::new();
        }
        let mut property_realizations =
            crate::jvm::property_realizations::PropertyRealizations::default();
        if let Err(target) =
            crate::jvm::local_properties::realize(&mut file.ir, &mut property_realizations)
        {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: cannot realize local JVM property access for {target:?}"),
            );
            return Vec::new();
        }
        if let Err(target) = crate::jvm::module_calls::realize(
            &mut file.ir,
            file.stems,
            &self.cp,
            &mut property_realizations,
        ) {
            diags.error(
                crate::diag::Span::new(0, 0),
                format!("internal error: missing JVM module layout for {target:?}"),
            );
            return Vec::new();
        }
        self.emit_streamed_ir(file, property_realizations, state, diags)
    }

    fn finalize(&self, state: JvmState, module_name: &str) -> Vec<Artifact> {
        // META-INF/<module>.kotlin_module — maps packages to their file-facade classes so Kotlin
        // consumers can resolve top-level declarations from the compiled module.
        // kotlinc writes the module file even for a CLASS-ONLY module (empty parts list), so emit
        // it unconditionally — omitting it byte-diverges the artifact set from the reference
        // compiler.
        let packages: Vec<(String, Vec<String>)> = state.module_packages.into_iter().collect();
        let module_bytes = crate::metadata::module::build_kotlin_module(&packages);
        vec![(
            crate::metadata::module::kotlin_module_file_name(module_name),
            module_bytes,
        )]
    }
}

/// Build file-facade metadata from common IR alone. Semantic declaration records were copied from
/// finalized Pass-1 headers before bodies streamed; this step combines them only with physical
/// function names/descriptors chosen by JVM representation passes.
pub fn facade_package_metadata_from_ir(
    ir: &crate::ir::IrFile,
    module_name: &str,
) -> Option<crate::jvm::ir_emit::KotlinMetadata> {
    let functions = ir
        .package_functions
        .iter()
        .map(|declaration| {
            let mentions_type_parameter = matches!(declaration.ret, Ty::TyParam(..))
                || declaration
                    .params
                    .iter()
                    .any(|(_, parameter)| matches!(parameter, Ty::TyParam(..)));
            let records_an_array = declaration
                .receiver
                .into_iter()
                .chain(declaration.params.iter().map(|(_, parameter)| *parameter))
                .chain(std::iter::once(declaration.ret))
                .any(crate::metadata::descriptor_needs_recording);
            let jvm_desc = (declaration.suspend
                || mentions_type_parameter
                || records_an_array
                || declaration.context_count > 0)
                .then(|| {
                    let mut physical = declaration
                        .params
                        .iter()
                        .map(|(_, parameter)| *parameter)
                        .collect::<Vec<_>>();
                    if let Some(receiver) = declaration.receiver {
                        physical.insert(declaration.context_count.min(physical.len()), receiver);
                    }
                    let mut descriptor = physical
                        .iter()
                        .map(|parameter| crate::jvm::names::type_descriptor(*parameter))
                        .collect::<String>();
                    if declaration.suspend {
                        descriptor.push_str("Lkotlin/coroutines/Continuation;");
                    }
                    format!(
                        "({descriptor}){}",
                        if declaration.suspend {
                            "Ljava/lang/Object;".to_owned()
                        } else {
                            crate::jvm::names::type_descriptor(declaration.ret)
                        }
                    )
                });
            let physical = ir.functions.get(declaration.function as usize);
            let jvm_name = physical
                .filter(|function| function.name != declaration.name)
                .map(|function| function.name.clone());
            let jvm_desc = if ir.vc_declared_sigs.contains_key(&declaration.function) {
                physical.map(|function| {
                    crate::jvm::names::method_descriptor(&function.params, function.ret)
                })
            } else {
                jvm_desc
            };
            let mut param_annotations = ir
                .fn_param_annotations
                .get(&declaration.function)
                .cloned()
                .unwrap_or_default();
            if declaration.receiver.is_some() && declaration.context_count < param_annotations.len()
            {
                param_annotations.remove(declaration.context_count);
            }
            let mut no_infer_params = ir
                .fn_param_no_infer
                .get(&declaration.function)
                .cloned()
                .unwrap_or_default();
            if declaration.receiver.is_some() && declaration.context_count < no_infer_params.len() {
                no_infer_params.remove(declaration.context_count);
            }
            crate::metadata::builder::FnMeta {
                name: declaration.name.clone(),
                params: declaration.params.clone(),
                ret: declaration.ret,
                decl_order: declaration.source_order as usize,
                annotations: ir
                    .function_annotations
                    .get(&declaration.function)
                    .map(|annotations| annotations.applications().cloned().collect())
                    .unwrap_or_default(),
                receiver: declaration.receiver,
                param_defaults: declaration.param_defaults.clone(),
                suspend: declaration.suspend,
                jvm_desc,
                jvm_name,
                inline: declaration.inline,
                operator: declaration.operator,
                infix: declaration.infix,
                contract: declaration
                    .contract
                    .as_ref()
                    .map(|contract| contract.to_arc()),
                type_params: declaration
                    .type_params
                    .iter()
                    .map(|parameter| (parameter.name.clone(), parameter.reified))
                    .collect(),
                semantic_type_params: declaration
                    .type_params
                    .iter()
                    .map(|parameter| parameter.semantic_name.clone())
                    .collect(),
                type_param_bounds: declaration
                    .type_params
                    .iter()
                    .map(|parameter| parameter.bounds.clone())
                    .collect(),
                context_count: declaration.context_count,
                vararg_index: declaration.vararg_index,
                visibility: declaration.visibility,
                spellings: declaration.spellings.clone(),
                param_annotations: param_annotations
                    .iter()
                    .map(|annotations| annotations.applications().cloned().collect())
                    .collect(),
                no_infer_params,
                equality_bound: declaration.equality_bound,
            }
        })
        .collect::<Vec<_>>();

    let properties = ir
        .package_properties
        .iter()
        .map(|declaration| {
            let accessor_parameters = declaration
                .context_parameters
                .iter()
                .copied()
                .chain(declaration.receiver)
                .collect::<Vec<_>>();
            let descriptor_parameters = accessor_parameters
                .iter()
                .map(|parameter| crate::jvm::names::type_descriptor(*parameter))
                .collect::<String>();
            let ty_descriptor = crate::jvm::names::type_descriptor(declaration.ty);
            let getter = (
                crate::jvm::names::property_getter_name(&declaration.name),
                format!("({descriptor_parameters}){ty_descriptor}"),
            );
            let setter = declaration.mutable.then(|| {
                let mut parameters = descriptor_parameters;
                parameters.push_str(&ty_descriptor);
                (
                    crate::jvm::names::property_setter_name(&declaration.name),
                    format!("({parameters})V"),
                )
            });
            crate::metadata::builder::PropMeta {
                name: declaration.name.clone(),
                ty: declaration.ty,
                is_var: declaration.mutable,
                type_params: declaration
                    .type_params
                    .iter()
                    .map(|parameter| parameter.name.clone())
                    .collect(),
                semantic_type_params: declaration
                    .type_params
                    .iter()
                    .map(|parameter| parameter.semantic_name.clone())
                    .collect(),
                type_param_bounds: declaration
                    .type_params
                    .iter()
                    .map(|parameter| parameter.bounds.clone())
                    .collect(),
                receiver: declaration.receiver,
                context_params: declaration
                    .context_parameter_names
                    .iter()
                    .cloned()
                    .zip(declaration.context_parameters.iter().copied())
                    .collect(),
                getter,
                setter,
                is_const: declaration.is_const,
                has_constant: declaration.has_constant,
                decl_order: declaration.source_order as usize,
                visibility: declaration.visibility,
                spellings: declaration.spellings.clone(),
                has_backing_field: declaration.has_backing_field,
                has_declared_getter: declaration.has_declared_getter,
            }
        })
        .collect::<Vec<_>>();

    let aliases = ir
        .package_type_aliases
        .iter()
        .map(|alias| crate::metadata::builder::TypeAliasMeta {
            name: alias.name.clone(),
            formals: alias.formals.clone(),
            expansion: alias.expansion,
            visibility: alias.visibility,
            expansion_spelling: alias.expansion_spelling.clone(),
            decl_order: alias.source_order as usize,
        })
        .collect::<Vec<_>>();
    build_facade_metadata(functions, properties, aliases, module_name)
}

fn build_facade_metadata(
    functions: Vec<crate::metadata::builder::FnMeta>,
    properties: Vec<crate::metadata::builder::PropMeta>,
    aliases: Vec<crate::metadata::builder::TypeAliasMeta>,
    module_name: &str,
) -> Option<crate::jvm::ir_emit::KotlinMetadata> {
    (!functions.is_empty() || !properties.is_empty() || !aliases.is_empty()).then(|| {
        let (d1_bytes, d2) = crate::metadata::builder::build_package(
            &functions,
            &properties,
            &aliases,
            (module_name != "main").then_some(module_name),
        );
        crate::jvm::ir_emit::KotlinMetadata {
            k: 2,
            mv: vec![2, 4, 0],
            xi: 48,
            d1: vec![d1_bytes.iter().map(|&byte| byte as char).collect()],
            d2,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every caller supplies a logical source stem, but module/corpus callers can retain directories
    /// that the CLI has already stripped. The shared constructor must own that normalization so all
    /// emitted `SourceFile` attributes contain the JVM-required simple filename on either path style.
    #[test]
    fn shipping_emit_options_normalize_logical_source_paths() {
        let cp = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(Vec::new()));
        let unix = shipping_emit_options("suite/nested/Foo", "main", None, cp.clone());
        let windows = shipping_emit_options("suite\\nested\\Bar", "main", None, cp);

        assert_eq!(unix.source_file.as_deref(), Some("Foo.kt"));
        assert_eq!(windows.source_file.as_deref(), Some("Bar.kt"));
    }

    /// These are not arbitrary emitter unit tests: each claims to compile or survey the bytes that
    /// krusty ships. Pin that architectural boundary so adding a new `EmitOptions` field cannot leave
    /// one of those pipelines on a subtly different artifact shape. A focused differential helper may
    /// mutate the returned options afterward (for example, force metadata despite a bisect env var),
    /// but it must still begin with the complete shared configuration.
    #[test]
    fn shipping_pipelines_do_not_reimplement_emit_options() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative in [
            "src/bin/survey.rs",
            "tests/common/mod.rs",
            "tests/kotlin_box_ir_jvm_conformance.rs",
        ] {
            let text =
                std::fs::read_to_string(root.join(relative)).expect("read shipping pipeline");
            assert!(
                !text.contains("EmitOptions {"),
                "{relative} must start from jvm::backend::shipping_emit_options instead of duplicating the shipping configuration",
            );
        }
    }

    /// JVM post-lowering passes must run through the checked streaming pipeline.
    #[test]
    fn backend_passes_are_only_called_via_checked_pipeline() {
        // token that marks a CALL of the pass → files allowed to contain it (the defining module's
        // internal/recursive uses, and the shared pipeline in this file).
        let rules: &[(&str, &[&str])] = &[
            (
                "realize_top_level_jvm_fields(",
                &["src/jvm/property_storage.rs", "src/jvm/backend.rs"],
            ),
            (
                "lower_companion_properties(",
                &["src/jvm/companion.rs", "src/jvm/backend.rs"],
            ),
            (
                "elide_default_property_stores(",
                &["src/jvm/property_storage.rs", "src/jvm/backend.rs"],
            ),
            (
                "lower_value_classes(",
                &["src/jvm/value_classes.rs", "src/jvm/backend.rs"],
            ),
            (
                "lower_suspend(",
                &["src/jvm/suspend.rs", "src/jvm/backend.rs"],
            ),
            (
                "mark_must_inline_lambdas(",
                &["src/jvm/ir_emit.rs", "src/jvm/backend.rs"],
            ),
            (
                "reparent_lambda_impls(",
                &["src/jvm/ir_emit.rs", "src/jvm/backend.rs"],
            ),
            (
                "run_enabled_from_ir(",
                &["src/plugins/mod.rs", "src/jvm/backend.rs"],
            ),
            (
                "derive_bridges(",
                &["src/jvm/bridges.rs", "src/jvm/backend.rs"],
            ),
            ("apply_collection_bridge_barriers(", &["src/jvm/backend.rs"]),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        for dir in ["src", "tests"] {
            visit(&root.join(dir), &mut |path, text| {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                for (token, allowed) in rules {
                    if text.contains(token) && !allowed.contains(&rel.as_str()) {
                        offenders.push(format!("{rel}: calls `{token}…)` directly"));
                    }
                }
            });
        }
        assert!(
            offenders.is_empty(),
            "backend passes must go through jvm::backend::run_backend_passes_with_checked_metadata (so a new pass lands \
             in every pipeline by construction), but:\n  {}",
            offenders.join("\n  ")
        );
    }

    fn visit(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                visit(&p, f);
            } else if p.extension().is_some_and(|x| x == "rs") {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    f(&p, &text);
                }
            }
        }
    }
}
