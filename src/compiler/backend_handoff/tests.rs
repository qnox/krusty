//! The dependency facts frozen at the backend boundary answer every identity the file's IR holds,
//! and each answer is what the provider normalized for that exact declaration.

use std::cell::RefCell;
use std::rc::Rc;

use crate::backend::{
    referenced_dependencies, Artifact, Backend, BackendCallableFact, CheckedBackendCallables,
    CheckedIrFile, DependencyFactError,
};
use crate::compiler::emit_analyzed;
use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::fir::ExternalCallableId;
use crate::jvm::classpath::Classpath;
use crate::libraries::ExternalCallableKind;
use crate::source::SourceInput;
use crate::types::type_name;

/// Two repository-owned dependencies publish the same top-level and member spellings. Distinct
/// identities, rather than a stdlib special case, must keep their frozen realizations apart.
const LIBRARY: &str = r#"package lib

fun collide(value: String): String = value

class Buffer {
    fun append(value: String): String = value
}

open class Base(val seed: Int) {
    var mutableSeed: Int = seed
    open fun greet(name: String = "x"): String = name
    open val tag: String get() = "base"
}
"#;

const OTHER_LIBRARY: &str = r#"package other

fun collide(value: String): String = value

class Buffer {
    fun append(value: String): String = value
}
"#;

const PROGRAM: &str = r#"import lib.Buffer as LibBuffer
import lib.collide as libCollide
import other.Buffer as OtherBuffer
import other.collide as otherCollide

fun box(): String {
    val left = LibBuffer().append(libCollide("O"))
    val right = OtherBuffer().append(otherCollide("K"))
    return left + right
}
"#;

/// Exercises every side-table source of a dependency identity: a primary and a secondary super
/// constructor, override edges of a function and a property, a module call that takes its omitted
/// arguments' defaults from a dependency declaration, and a delegate convention.
const SOURCES: &str = r#"import lib.Base

class Direct : Base(2)

class Child : Base {
    constructor() : super(1)
    override fun greet(name: String): String = name + seed
    override val tag: String get() = "child"
}

fun greetings(): String = Child().greet()

val lazyValue: Int by lazy { 3 }
"#;

/// Every callable fact of the file, with its identity, in identity order.
type Recorded = Vec<(ExternalCallableId, BackendCallableFact)>;

/// Each side-table source of a dependency identity, read straight off the checked IR the backend
/// receives, and the identities it carries.
fn side_table_carriers(ir: &crate::ir::IrFile) -> Vec<(&'static str, ExternalCallableId)> {
    use crate::fir::{
        FirCallTarget, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget,
    };
    let mut carriers = Vec::new();
    for target in ir.external_super_constructors.values() {
        carriers.push(("super constructor", target.declaration));
    }
    for target in ir.external_secondary_super_constructors.values() {
        carriers.push(("secondary super constructor", target.declaration));
    }
    for edge in ir.function_overrides.values().flatten() {
        for target in [edge.implementation, edge.overridden] {
            if let ResolvedFunctionOverrideTarget::External(callable) = target {
                carriers.push(("function override", callable));
            }
        }
    }
    for edge in ir.property_overrides.values().flatten() {
        for target in [edge.implementation, edge.overridden] {
            if let ResolvedPropertyOverrideTarget::External(accessor) = target {
                carriers.push(("property override", accessor));
            }
        }
    }
    for expression in &ir.exprs {
        if let crate::ir::IrExpr::Call {
            callee:
                crate::ir::Callee::ModuleWithDefaults {
                    default_provider: ResolvedFunctionOverrideTarget::External(provider),
                    ..
                },
            ..
        } = expression
        {
            carriers.push(("default provider", *provider));
        }
    }
    for plan in ir
        .checked_properties
        .values()
        .filter_map(|property| property.delegate_plan.as_ref())
    {
        for call in plan
            .provide_delegate
            .iter()
            .chain([&plan.get_value])
            .chain(&plan.set_value)
        {
            if let FirCallTarget::External { declaration, .. } = call.target {
                carriers.push(("delegate convention", declaration));
            }
        }
    }
    carriers
}

/// Records the frozen facts of every file after checking them against the provider's own records.
struct FactRecorder {
    classpath: Rc<Classpath>,
    callables: RefCell<Recorded>,
    carriers: RefCell<Vec<(&'static str, ExternalCallableId)>>,
}

impl Backend for FactRecorder {
    type State = ();

    fn lower_ir_file(
        &self,
        file: CheckedIrFile<'_>,
        _state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> Vec<Artifact> {
        let referenced = referenced_dependencies(&file.ir);
        let mut held = referenced.callables.clone();
        for property in referenced.properties {
            let realization = self
                .classpath
                .external_property(property)
                .expect("the provider answers for its own property identity");
            held.insert(realization.getter);
            held.extend(realization.setter);
        }
        self.carriers
            .borrow_mut()
            .extend(side_table_carriers(&file.ir));
        for callable in &held {
            let fact = file
                .callables
                .callable(*callable)
                .expect("every referenced callable has a frozen fact");
            let realization = self
                .classpath
                .external_callable(*callable)
                .expect("the provider answers for its own callable identity");
            let declaration = realization.callable;
            assert_eq!(fact.name, declaration.name);
            assert_eq!(fact.reflection_name, declaration.reflection_name);
            assert_eq!(fact.physical_owner, declaration.owner);
            assert_eq!(fact.kind, realization.kind);
            assert_eq!(fact.owner_is_interface, declaration.owner_is_interface);
            assert_eq!(fact.compiler_intrinsic, declaration.compiler_intrinsic);
            assert_eq!(fact.semantic_role, declaration.semantic_role);
            assert_eq!(fact.member_realization, declaration.member_realization);
            assert_eq!(fact.params, declaration.params);
            assert_eq!(fact.physical_params, declaration.physical_params);
            assert_eq!(fact.physical_ret, declaration.physical_ret);
            assert_eq!(fact.descriptor, declaration.descriptor);
            assert_eq!(fact.inline, declaration.inline);
            assert_eq!(fact.source_receiver, declaration.source_receiver);
            assert_eq!(fact.context_count, declaration.context_count);
            assert_eq!(fact.declared_params, declaration.declared_params);
            assert_eq!(fact.inline_modifiers, declaration.inline_modifiers);
            assert_eq!(
                fact.nonvirtual_realization,
                declaration.nonvirtual_realization
            );
            assert_eq!(fact.generic_sig, declaration.generic_sig);
            assert_default_realization(
                fact.default_realization.as_deref(),
                declaration.default_realization.as_deref(),
            );
            self.callables.borrow_mut().push((*callable, fact.clone()));
        }
        let mut unreferenced = 0;
        for raw in 0.. {
            let identity = ExternalCallableId::from_raw(raw);
            if self.classpath.external_callable(identity).is_none() {
                break;
            }
            if !held.contains(&identity) {
                assert!(file.callables.callable(identity).is_none());
                unreferenced += 1;
            }
        }
        assert!(
            unreferenced > 0,
            "the provider knows declarations this file never selected"
        );
        Vec::new()
    }

    fn finalize(&self, _state: Self::State, _module_name: &str) -> Vec<Artifact> {
        Vec::new()
    }
}

fn platform_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    paths.extend(crate::jvm::kotlin_stdlib_jar());
    paths.extend(crate::jvm::classpath::platform_jdk_modules(None));
    paths
}

fn analyze(
    source: &str,
    stem: &str,
    classpath: &Rc<Classpath>,
    diagnostics: &mut DiagSink,
) -> crate::frontend::SourceSetAnalysis {
    let inputs = [SourceInput::kotlin(source).with_file_stem(stem)];
    let stems = [stem.to_string()];
    crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
                .expect("JVM provider initialization"),
        ),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        diagnostics,
    )
}

fn analyze_many(
    sources: &[(&str, &str)],
    classpath: &Rc<Classpath>,
    diagnostics: &mut DiagSink,
) -> (crate::frontend::SourceSetAnalysis, Vec<String>) {
    let inputs = sources
        .iter()
        .map(|(source, stem)| SourceInput::kotlin(*source).with_file_stem(*stem))
        .collect::<Vec<_>>();
    let stems = sources
        .iter()
        .map(|(_, stem)| (*stem).to_string())
        .collect::<Vec<_>>();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
                .expect("JVM provider initialization"),
        ),
        &LangFeatures::new(),
        |files, symbols| crate::jvm::prepare_module_symbols(files, &stems, symbols),
        diagnostics,
    );
    (analysis, stems)
}

/// Compile the repository-owned library with krusty and return the directory holding its classes.
fn compile_library() -> std::path::PathBuf {
    let classpath = Rc::new(Classpath::new(platform_paths()));
    let mut diagnostics = DiagSink::new();
    let (analysis, stems) = analyze_many(
        &[(LIBRARY, "Lib"), (OTHER_LIBRARY, "Other")],
        &classpath,
        &mut diagnostics,
    );
    let artifacts = emit_analyzed(
        analysis,
        &stems,
        &crate::jvm::JvmBackend::new(classpath),
        "lib",
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let directory = std::env::temp_dir().join(format!(
        "krusty-dependency-facts-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    for (path, bytes) in artifacts {
        let path = directory.join(path);
        std::fs::create_dir_all(path.parent().expect("an artifact has a directory"))
            .expect("create the library class directory");
        std::fs::write(path, bytes).expect("write a library class");
    }
    directory
}

/// Freeze the program's dependency facts over the stdlib, the JDK and the compiled library.
fn frozen_program_facts() -> Recorded {
    frozen_facts(PROGRAM).0
}

/// Freeze `program`'s dependency facts, and report every side-table carrier the backend saw.
fn frozen_facts(program: &str) -> (Recorded, Vec<(&'static str, ExternalCallableId)>) {
    let library = compile_library();
    let mut paths = platform_paths();
    paths.push(library.clone());
    let classpath = Rc::new(Classpath::new(paths));
    let mut diagnostics = DiagSink::new();
    let analysis = analyze(program, "Program", &classpath, &mut diagnostics);
    let recorder = FactRecorder {
        classpath,
        callables: RefCell::default(),
        carriers: RefCell::default(),
    };
    let outputs = emit_analyzed(
        analysis,
        &["Program".to_string()],
        &recorder,
        "main",
        &mut diagnostics,
    );
    std::fs::remove_dir_all(library).expect("remove the library class directory");
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(outputs.is_empty());
    (
        recorder.callables.into_inner(),
        recorder.carriers.into_inner(),
    )
}

/// Assert that the facts published under `name` are exactly `expected`, in any identity order.
fn assert_default_realization(
    actual: Option<&crate::libraries::DefaultCallRealization>,
    expected: Option<&crate::libraries::DefaultCallRealization>,
) {
    let (Some(actual), Some(expected)) = (actual, expected) else {
        assert_eq!(actual.is_some(), expected.is_some());
        return;
    };
    assert_eq!(actual.owner, expected.owner);
    assert_eq!(actual.name, expected.name);
    assert_eq!(actual.descriptor, expected.descriptor);
    assert_eq!(actual.declaration_owner, expected.declaration_owner);
    assert_eq!(actual.real_params, expected.real_params);
    assert_eq!(actual.mask_count, expected.mask_count);
    assert_eq!(actual.ret, expected.ret);
    assert_eq!(actual.suspend, expected.suspend);
}

fn fact_name(fact: &BackendCallableFact) -> &str {
    fact.reflection_name.as_deref().unwrap_or(&fact.name)
}

fn assert_named(
    facts: &Recorded,
    name: &str,
    expected: &[(crate::types::TypeName, ExternalCallableKind)],
) {
    let named = facts
        .iter()
        .map(|(_, fact)| fact)
        .filter(|fact| fact_name(fact) == name)
        .map(|fact| (fact.physical_owner, fact.kind))
        .collect::<Vec<_>>();
    assert_eq!(named.len(), expected.len(), "facts named {name}: {named:?}");
    for fact in expected {
        assert!(named.contains(fact), "{fact:?} is not among {named:?}");
    }
}

#[test]
fn frozen_facts_answer_every_dependency_identity_the_ir_references() {
    let callables = frozen_program_facts();
    let mut summary = callables
        .iter()
        .map(|(_, fact)| (Some(fact_name(fact)), fact.kind))
        .collect::<Vec<_>>();
    summary.sort_by_key(|(name, kind)| (*name, format!("{kind:?}")));
    assert_eq!(
        summary,
        [
            // `lib.Buffer()` and `other.Buffer()`.
            (Some("<init>"), ExternalCallableKind::Constructor),
            (Some("<init>"), ExternalCallableKind::Constructor),
            // The two repository-owned `Buffer.append` declarations.
            (Some("append"), ExternalCallableKind::Member),
            (Some("append"), ExternalCallableKind::Member),
            // The two repository-owned top-level declarations.
            (Some("collide"), ExternalCallableKind::TopLevel),
            (Some("collide"), ExternalCallableKind::TopLevel),
        ]
    );
}

#[test]
fn same_named_repository_top_level_functions_have_distinct_facts() {
    let callables = frozen_program_facts();
    assert_named(
        &callables,
        "collide",
        &[
            (type_name("lib/LibKt"), ExternalCallableKind::TopLevel),
            (type_name("other/OtherKt"), ExternalCallableKind::TopLevel),
        ],
    );
}

#[test]
fn same_named_repository_members_have_distinct_facts() {
    let callables = frozen_program_facts();
    assert_named(
        &callables,
        "append",
        &[
            (type_name("lib/Buffer"), ExternalCallableKind::Member),
            (type_name("other/Buffer"), ExternalCallableKind::Member),
        ],
    );
}

#[test]
fn every_side_table_source_of_a_dependency_callable_is_frozen() {
    let (callables, carriers) = frozen_facts(SOURCES);
    let frozen = callables
        .iter()
        .map(|(identity, _)| *identity)
        .collect::<std::collections::BTreeSet<_>>();
    let mut sources = carriers
        .iter()
        .map(|(source, _)| *source)
        .collect::<Vec<_>>();
    sources.sort_unstable();
    sources.dedup();
    // The program exercises each source, so a source that stopped reaching the backend would drop
    // out of this list rather than pass unnoticed.
    assert_eq!(
        sources,
        [
            "default provider",
            "delegate convention",
            "function override",
            "property override",
            "secondary super constructor",
            "super constructor",
        ]
    );
    for (source, identity) in &carriers {
        assert!(
            frozen.contains(identity),
            "the {source} {identity:?} has no frozen fact"
        );
    }
}

#[test]
fn dependency_property_accessors_are_frozen_before_backend_realization() {
    let (callables, _) = frozen_facts(
        r#"import lib.Base

fun update(base: Base): Int {
    base.mutableSeed = base.seed
    return base.mutableSeed
}
"#,
    );
    assert_named(
        &callables,
        "getSeed",
        &[(type_name("lib/Base"), ExternalCallableKind::Member)],
    );
    assert_named(
        &callables,
        "getMutableSeed",
        &[(type_name("lib/Base"), ExternalCallableKind::Member)],
    );
    assert_named(
        &callables,
        "setMutableSeed",
        &[(type_name("lib/Base"), ExternalCallableKind::Member)],
    );
}

#[test]
fn an_identity_its_provider_cannot_answer_is_an_internal_error() {
    let mut ir = crate::ir::IrFile::default();
    let target = ExternalCallableId::from_raw(3);
    ir.add_expr(crate::ir::IrExpr::Call {
        callee: crate::ir::Callee::External {
            target,
            default_provider: None,
            params: Vec::new(),
            ret: crate::types::Ty::Unit,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        },
        dispatch_receiver: None,
        args: Vec::new(),
    });
    assert_eq!(
        CheckedBackendCallables::freeze(&ir, &crate::libraries::EmptySymbolSource).map(|_| ()),
        Err(DependencyFactError::UnknownCallable(target))
    );
}
