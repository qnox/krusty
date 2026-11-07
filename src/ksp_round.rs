//! What a KSP processor reads in one round, built from the module's resolved declaration headers.
//! See `docs/PLUGIN_API.md`.
//!
//! KSP processors only read declarations and write new files, so a round needs the module's
//! headers, never its bodies: a driver analyzes the sources for declarations only, hands the
//! [`KspRound`] built here to a KSP host (`krusty-plugin-host`'s `KspHost`), adds the sources the
//! processors generate, and analyzes again for the next round. The compiler itself never starts the
//! host.
//!
//! The round carries what the host serves so far. Where a declaration has more than the round can
//! describe, it says so (`declarations_complete`), and the host refuses a processor that asks for
//! the rest rather than answer with a part of it.

mod accessors;
mod all_members;
mod annotations;
mod backing_field;
mod declarations;
mod library;
mod overridees;
mod platform_names;
mod type_model;

pub use library::KspLibraries;

use krusty_plugin_ir::{
    KspAnnotation, KspClass, KspClassKind, KspDeclaration, KspFile, KspRound, KspType,
    KspTypeAlias, KspTypeParameter,
};

use crate::ast::{AnnotationRef, ClassDecl, ClassKind, Decl, DeclId, File};
use crate::frontend::{
    FrontendClassSig, FrontendDeclarationId, FrontendSymbols, Signature, SourceSetAnalysis,
};
use crate::types::Ty;
use platform_names::PlatformNames;
use type_model::{class_type, modifiers, qualified_name, TypeParameterIds, TypeParameterScope};

/// The round a processor reads from `analysis`, whose files are at `paths` (one per source, in the
/// analysis's order), for the module `module_name`. `analysis` collects every file's signatures and checks no body
/// (`analyze_source_set_prefix_with_features(sources, 0, sources.len(), …)`). A declaration the
/// round cannot name, such as an annotation that did not resolve, is refused by what it is and
/// where.
pub fn ksp_round(
    analysis: &SourceSetAnalysis,
    paths: &[String],
    module_name: &str,
) -> Result<KspRound, String> {
    if paths.len() != analysis.files.len() {
        return Err(format!(
            "KSP needs the path of every source: {} paths were given for {} sources",
            paths.len(),
            analysis.files.len()
        ));
    }
    let resolved = Resolved::new(analysis);
    let ids = TypeParameterIds::default();
    let platform = PlatformNames::new(analysis, module_name);
    let ledger = all_members::Ledger::default();
    let mut files = Vec::with_capacity(paths.len());
    for (index, (file, path)) in analysis.files.iter().zip(paths).enumerate() {
        let source = Source {
            index: index as u32,
            file,
            path,
            analysis,
            symbols: &analysis.symbols,
            resolved: &resolved,
            ids: &ids,
            platform: platform.as_ref(),
            ledger: &ledger,
        };
        files.push(source.file()?);
    }
    let ledger = ledger.into_classes();
    all_members::order(&mut files, &ledger, analysis);
    overridees::assign(&mut files, &ledger, analysis);
    Ok(KspRound { files })
}

/// The resolved signature of each source declaration, by where it is declared.
struct Resolved<'a> {
    /// Each classifier, by file and declaration.
    classes: crate::fx_hash::HashMap<(u32, DeclId), &'a FrontendClassSig>,
    /// Each top-level function, by file and declaration.
    functions: crate::fx_hash::HashMap<(u32, DeclId), &'a Signature>,
    /// Each member function, by its classifier's declaration and its index among the classifier's
    /// written functions.
    methods: crate::fx_hash::HashMap<(FrontendDeclarationId, u32), &'a Signature>,
    /// Each member function, by its declaration.
    member_functions: crate::fx_hash::HashMap<FrontendDeclarationId, &'a Signature>,
}

impl<'a> Resolved<'a> {
    fn new(analysis: &'a SourceSetAnalysis) -> Self {
        let symbols = &analysis.symbols;
        let classes = symbols
            .classes
            .values()
            .filter_map(|signature| {
                Some(((signature.source_file, signature.source_decl?), signature))
            })
            .collect();
        let functions = symbols
            .funs
            .values()
            .flatten()
            .chain(
                symbols
                    .ext_funs
                    .values()
                    .flat_map(|by_receiver| by_receiver.values())
                    .flatten(),
            )
            .filter_map(|signature| {
                Some(((signature.source_file?, signature.source_decl?), signature))
            })
            .collect();
        let member_functions: crate::fx_hash::HashMap<_, _> = symbols
            .classes
            .values()
            .flat_map(|class| {
                class.methods.values().flatten().chain(
                    class
                        .member_ext_funs
                        .values()
                        .flatten()
                        .map(|function| function.signature()),
                )
            })
            .filter_map(|signature| Some((signature.stable_declaration?, signature)))
            .collect();
        let methods = member_functions
            .iter()
            .filter_map(|(&declaration, &signature)| {
                Some((analysis.member_function_slot(declaration)?, signature))
            })
            .collect();
        Resolved {
            classes,
            functions,
            methods,
            member_functions,
        }
    }
}

/// One analyzed source file, and what its declarations resolved to.
struct Source<'a> {
    index: u32,
    file: &'a File,
    path: &'a str,
    analysis: &'a SourceSetAnalysis,
    symbols: &'a FrontendSymbols,
    resolved: &'a Resolved<'a>,
    ids: &'a TypeParameterIds,
    /// The names the platform's binary interface gives declarations, if it has one.
    platform: Option<&'a PlatformNames<'a>>,
    /// What each classifier's member lists are built from, once every file is described.
    ledger: &'a all_members::Ledger,
}

/// A declaration a refusal names: what it is, its name and its line.
#[derive(Clone, Copy)]
struct Subject<'a> {
    kind: &'static str,
    name: &'a str,
    line: u32,
}

impl Source<'_> {
    fn file(&self) -> Result<KspFile, String> {
        let package_name = self.file.package.clone().unwrap_or_default();
        let root = TypeParameterScope::default();
        let mut callables = Vec::new();
        let mut classifiers = Vec::new();
        for &id in &self.file.decls {
            if !self.is_member_of(id, None) {
                continue;
            }
            match self.file.decl(id) {
                Decl::Class(class) => {
                    classifiers.push((
                        class.span.lo,
                        KspDeclaration::Class(self.class(id, class, &root)?),
                    ));
                }
                Decl::Fun(function) => {
                    let owner = Owner::TopLevel {
                        package: &package_name,
                        id,
                    };
                    callables.push(KspDeclaration::Function(
                        self.function(function, &owner, &root)?,
                    ));
                }
                Decl::Property(property) => {
                    let owner = Owner::TopLevel {
                        package: &package_name,
                        id,
                    };
                    callables.push(KspDeclaration::Property(Box::new(
                        self.property(property, &owner, &root)?,
                    )));
                }
            }
        }
        for alias in &self.file.type_alias_decls {
            let qualified_name = if package_name.is_empty() {
                alias.name.clone()
            } else {
                format!("{package_name}.{}", alias.name)
            };
            classifiers.push((
                alias.span.lo,
                KspDeclaration::TypeAlias(KspTypeAlias {
                    simple_name: alias.name.clone(),
                    qualified_name,
                }),
            ));
        }
        classifiers.sort_by_key(|(start, _)| *start);
        let mut declarations = callables;
        declarations.extend(classifiers.into_iter().map(|(_, declaration)| declaration));
        Ok(KspFile {
            path: self.path.to_string(),
            package_name,
            declarations,
            // Every top-level declaration KSP lists is one the round describes.
            declarations_complete: true,
        })
    }

    /// Whether `id` is declared directly in `owner`'s body, or at the top level when `owner` is
    /// `None`. Local and anonymous classifiers are in neither: KSP does not list them. Nor is a
    /// classifier in an enum entry's body, which the round does not describe: its enum entry says
    /// its list is incomplete.
    fn is_member_of(&self, id: DeclId, owner: Option<DeclId>) -> bool {
        if self
            .file
            .enum_entry_nested_classifier_owners
            .contains_key(&id)
            || self.file.local_declarations.contains(&id)
            || self
                .file
                .local_class_enclosing_declarations
                .contains_key(&id)
            || self.file.is_anonymous_object_class(id)
        {
            return false;
        }
        self.file.hoisted_classifier_owners.get(&id).copied() == owner
    }

    fn class(
        &self,
        id: DeclId,
        class: &ClassDecl,
        enclosing: &TypeParameterScope,
    ) -> Result<KspClass, String> {
        let subject = Subject {
            kind: "class",
            name: &class.name,
            line: class.decl_line,
        };
        let Some(signature) = self.resolved.classes.get(&(self.index, id)) else {
            return Err(self.refusal(subject, "has no resolved declaration"));
        };
        // The resolved identity names the classifier, a companion object's implicit name included.
        let simple_name = match signature.internal.nested_owner() {
            Some(_) => signature.internal.nested_segment_ref().to_string(),
            None => signature.internal.segment(),
        };
        let qualified_name = qualified_name(signature.internal);
        let kind = match class.kind {
            ClassKind::Interface => KspClassKind::Interface,
            ClassKind::Enum => KspClassKind::EnumClass,
            ClassKind::Annotation => KspClassKind::AnnotationClass,
            ClassKind::Class if class.singleton => KspClassKind::Object,
            ClassKind::Class => KspClassKind::Class,
        };
        let scope = enclosing.with(
            self.ids,
            &qualified_name,
            signature
                .type_params()
                .iter()
                .zip(class.type_parameters.type_params())
                .map(|(identity, name)| (identity.as_str(), name.as_str())),
        );
        let annotations = self.published_annotations(
            subject,
            &class.annotations,
            Some(&signature.applied_annotations),
            annotations::Placement::Declaration,
        )?;
        let type_parameters = type_parameters(class, signature, &scope);
        let header = declarations::ClassHeader {
            id,
            file: self.file,
            class,
            signature,
            kind,
            qualified_name: &qualified_name,
            type_parameters: &type_parameters,
            scope: &scope,
            primary_spellings: signature
                .stable_declaration
                .and_then(|class| self.analysis.constructor_declaration(class, 0))
                .and_then(|constructor| self.analysis.declared_type_spellings(constructor)),
        };
        let described = self.class_declarations(&header)?;
        self.ledger.record(
            qualified_name.clone(),
            all_members::ClassLedger {
                resolved: described.resolved,
                generated: self.data_object_members(&header),
            },
        );
        Ok(KspClass {
            simple_name,
            qualified_name: qualified_name.clone(),
            kind,
            line: class.decl_line,
            annotations,
            modifiers: self.written_modifiers(class.name_span.lo),
            type_parameters,
            super_types: super_types(signature, &scope),
            declarations: described.declarations,
            declarations_complete: described.complete,
            jvm_signature: self
                .platform
                .and_then(|platform| platform.type_signature(Ty::obj_name(signature.internal))),
            // Filled in once every file is described, as they depend on the supertypes'.
            all_properties: None,
            all_functions: None,
        })
    }

    /// The KSP modifiers of the modifier list of the declaration that `offset` identifies.
    fn written_modifiers(&self, offset: u32) -> Option<Vec<krusty_plugin_ir::KspModifier>> {
        self.file
            .declaration_prefixes
            .at(offset)
            .map(|prefix| modifiers(prefix.modifiers))
    }

    fn annotations(
        &self,
        subject: Subject<'_>,
        annotations: &[AnnotationRef],
    ) -> Result<Vec<KspAnnotation>, String> {
        annotations
            .iter()
            .map(|annotation| self.annotation(subject, annotation))
            .collect()
    }

    fn annotation(
        &self,
        subject: Subject<'_>,
        annotation: &AnnotationRef,
    ) -> Result<KspAnnotation, String> {
        let Some(resolved) = self.symbols.resolved_annotation(self.index, annotation) else {
            return Err(self.refusal(
                subject,
                &format!(
                    "has the annotation @{}, which did not resolve",
                    annotation.name
                ),
            ));
        };
        Ok(KspAnnotation {
            short_name: annotations::short_name(resolved),
            qualified_name: qualified_name(resolved),
            use_site: None,
            synthetic: false,
            // An enum entry's applications are not published with their arguments yet.
            arguments: None,
        })
    }

    fn refusal(&self, subject: Subject<'_>, what: &str) -> String {
        format!(
            "{}:{}: KSP cannot read {} {}: it {what}",
            self.path, subject.line, subject.kind, subject.name
        )
    }
}

/// Where a function or property is declared.
enum Owner<'a> {
    TopLevel {
        package: &'a str,
        id: DeclId,
    },
    Class {
        header: &'a declarations::ClassHeader<'a>,
        /// Its index among the classifier's methods, for a function.
        method: Option<u32>,
    },
}

impl Owner<'_> {
    fn qualified_name(&self, name: &str) -> String {
        match self {
            Owner::TopLevel { package: "", .. } => name.to_string(),
            Owner::TopLevel { package, .. } => format!("{package}.{name}"),
            Owner::Class { header, .. } => format!("{}.{name}", header.qualified_name),
        }
    }
}

/// The classifier's type parameters with their upper bounds in written order, or `None` when one
/// does not have the shape the resolved declaration has or a bound is not a type KSP's model here
/// describes.
fn type_parameters(
    class: &ClassDecl,
    signature: &FrontendClassSig,
    scope: &TypeParameterScope,
) -> Option<Vec<KspTypeParameter>> {
    let names = class.type_parameters.type_params();
    if names.len() != signature.type_params().len()
        || signature.type_parameter_extra_bounds.len() != names.len()
    {
        return None;
    }
    signature
        .type_params()
        .iter()
        .zip(signature.type_param_bounds())
        .zip(signature.type_param_variances())
        .zip(&signature.type_parameter_extra_bounds)
        .map(|(((identity, &first), &variance), extra)| {
            scope.type_parameter(
                identity,
                variance,
                false,
                std::iter::once(first).chain(extra.iter().copied()),
            )
        })
        .collect()
}

/// The classifier's direct supertypes in written order, or `kotlin.Any` when it writes none.
fn super_types(signature: &FrontendClassSig, scope: &TypeParameterScope) -> Option<Vec<KspType>> {
    if signature.interface_type_args.len() != signature.interfaces.len() {
        return None;
    }
    let interfaces: Vec<_> = signature
        .interfaces
        .iter()
        .zip(&signature.interface_type_args)
        .collect();
    let before = (signature.interfaces_before_superclass as usize).min(interfaces.len());
    let superclass = signature
        .super_internal
        .map(|class| (class, &signature.super_type_args));
    let mut super_types = Vec::with_capacity(interfaces.len() + 1);
    for (class, arguments) in interfaces[..before]
        .iter()
        .copied()
        .chain(superclass)
        .chain(interfaces[before..].iter().copied())
    {
        super_types.push(scope.applied(class, arguments)?);
    }
    if super_types.is_empty() {
        super_types.push(class_type("kotlin.Any"));
    }
    Some(super_types)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::features::LangFeatures;
    use crate::frontend::{analyze_source_set_prefix_with_features, PlatformProvider, SourceInput};
    use crate::libraries::EmptySymbolSource;
    use krusty_plugin_ir::{
        KspFunction, KspFunctionKind, KspParameter, KspProperty, KspTypeArgument,
        KspTypeDeclaration, KspTypeSlot,
    };

    fn round(sources: &[(&str, &str)]) -> Result<KspRound, String> {
        let inputs: Vec<_> = sources
            .iter()
            .map(|(_, text)| SourceInput::kotlin(text))
            .collect();
        let mut diagnostics = DiagSink::new();
        let analysis = analyze_source_set_prefix_with_features(
            &inputs,
            0,
            inputs.len(),
            PlatformProvider::jvm(Box::new(EmptySymbolSource)),
            &LangFeatures::new(),
            &mut diagnostics,
        );
        let paths: Vec<_> = sources.iter().map(|(path, _)| path.to_string()).collect();
        ksp_round(&analysis, &paths, "main")
    }

    fn render(ty: &KspType) -> String {
        let mut out = match &ty.declaration {
            KspTypeDeclaration::Class(name) => name.clone(),
            KspTypeDeclaration::TypeAlias { qualified_name, .. } => qualified_name.clone(),
            KspTypeDeclaration::TypeParameter { owner, name, .. } => format!("{owner}.{name}"),
        };
        if !ty.arguments.is_empty() {
            let arguments: Vec<String> = ty
                .arguments
                .iter()
                .map(|argument| match argument {
                    KspTypeArgument::Star => "*".to_string(),
                    KspTypeArgument::Invariant(ty) => render(ty),
                    KspTypeArgument::Out(ty) => format!("out {}", render(ty)),
                    KspTypeArgument::In(ty) => format!("in {}", render(ty)),
                })
                .collect();
            out = format!("{out}<{}>", arguments.join(", "));
        }
        if ty.nullable {
            out.push('?');
        }
        out
    }

    fn rendered(ty: &Option<KspType>) -> String {
        ty.as_ref().map_or("<undescribed>".to_string(), render)
    }

    fn modifiers(modifiers: &Option<Vec<krusty_plugin_ir::KspModifier>>) -> String {
        let mut names: Vec<&str> = modifiers
            .as_ref()
            .expect("modifiers")
            .iter()
            .map(|modifier| modifier.name())
            .collect();
        names.sort_unstable();
        names.join(",")
    }

    fn kind(class: &KspClass) -> &'static str {
        match class.kind {
            KspClassKind::Interface => "INTERFACE",
            KspClassKind::Class => "CLASS",
            KspClassKind::EnumClass => "ENUM_CLASS",
            KspClassKind::EnumEntry => "ENUM_ENTRY",
            KspClassKind::Object => "OBJECT",
            KspClassKind::AnnotationClass => "ANNOTATION_CLASS",
        }
    }

    fn parameter(parameter: &KspParameter) -> String {
        format!(
            "{}: {} vararg={} default={}",
            parameter.name,
            rendered(&parameter.ty),
            parameter.is_vararg,
            parameter
                .has_default
                .map_or("<undescribed>".to_string(), |has_default| has_default
                    .to_string())
        )
    }

    fn function(function: &KspFunction) -> String {
        let receiver = match &function.extension_receiver {
            KspTypeSlot::Absent => "null".to_string(),
            KspTypeSlot::Present(ty) => render(ty),
            KspTypeSlot::Undescribed => "<undescribed>".to_string(),
        };
        let type_parameters: Vec<&str> = function
            .type_parameters
            .as_ref()
            .expect("type parameters")
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect();
        let parameters: Vec<String> = function.parameters.iter().map(parameter).collect();
        let annotations: String = function
            .annotations
            .iter()
            .map(|annotation| format!(" @{}", annotation.short_name))
            .collect();
        format!(
            "fun {} kind={} abstract={} modifiers={} typeParameters={} receiver={receiver} ({}): {}{annotations}",
            function.name,
            match function.kind {
                KspFunctionKind::TopLevel => "TOP_LEVEL",
                KspFunctionKind::Static => "STATIC",
                _ => "MEMBER",
            },
            function.is_abstract,
            modifiers(&function.modifiers),
            type_parameters.join(","),
            parameters.join(", "),
            rendered(&function.return_type)
        )
    }

    fn property(property: &KspProperty) -> String {
        let receiver = match &property.extension_receiver {
            KspTypeSlot::Absent => String::new(),
            KspTypeSlot::Present(ty) => format!(" receiver={}", render(ty)),
            KspTypeSlot::Undescribed => " receiver=<undescribed>".to_string(),
        };
        format!(
            "property {} mutable={} backingField={} modifiers={}: {}{receiver}",
            property.name,
            property.is_mutable,
            property
                .has_backing_field
                .map_or("<refused>".to_string(), |has| has.to_string()),
            modifiers(&property.modifiers),
            rendered(&property.ty)
        )
    }

    fn class_line(class: &KspClass) -> String {
        let type_parameters: Vec<String> = class
            .type_parameters
            .as_ref()
            .expect("type parameters")
            .iter()
            .map(|parameter| {
                let bounds: Vec<String> = parameter.bounds.iter().map(render).collect();
                format!("{}:{}", parameter.name, bounds.join("&"))
            })
            .collect();
        let super_types: Vec<String> = class
            .super_types
            .as_ref()
            .expect("supertypes")
            .iter()
            .map(render)
            .collect();
        format!(
            "{} {} modifiers={} typeParameters={} supertypes={}",
            kind(class),
            class.qualified_name,
            modifiers(&class.modifiers),
            type_parameters.join(","),
            super_types.join(",")
        )
    }

    /// The lines the `Members.kt` test processor logs for `declarations`.
    fn dump(declarations: &[KspDeclaration], indent: &str, out: &mut Vec<String>) {
        for declaration in declarations {
            match declaration {
                KspDeclaration::Class(class) => {
                    out.push(format!("{indent}{}", class_line(class)));
                    assert!(class.declarations_complete, "{}", class.qualified_name);
                    dump(&class.declarations, &format!("{indent}  "), out);
                }
                KspDeclaration::Function(declared) => {
                    out.push(format!("{indent}{}", function(declared)));
                }
                KspDeclaration::Property(declared) => {
                    out.push(format!("{indent}{}", property(declared)));
                }
                KspDeclaration::TypeAlias(alias) => {
                    out.push(format!("{indent}KSTypeAlias {}", alias.simple_name));
                }
                KspDeclaration::LibraryClass(name) => {
                    panic!("a source file declares the library classifier {name}");
                }
            }
        }
    }

    fn dumped(round: &KspRound) -> Vec<String> {
        let mut out = Vec::new();
        for file in &round.files {
            assert!(file.declarations_complete, "{}", file.path);
            dump(&file.declarations, "", &mut out);
        }
        out
    }

    #[test]
    fn a_round_lists_classifiers_nested_under_their_owners_with_resolved_annotations() {
        let round = round(&[
            (
                "/src/Sample.kt",
                "package sample\n\nannotation class Marker\n\n/** Documented. */\n@Marker\nclass Plain {\n    interface Inner\n\n    @Marker\n    object Holder\n}\n\ninterface Shape\n\n@Marker object Single\n\ninternal annotation class Tag\n",
            ),
            (
                "/src/Other.kt",
                "package sample.other\n\n@sample.Marker\n@sample.Tag\nclass Root\n",
            ),
        ])
        .expect("every declaration is readable");
        fn list<'a>(classes: impl Iterator<Item = &'a KspClass>, out: &mut Vec<String>) {
            for class in classes {
                let annotations: Vec<String> = class
                    .annotations
                    .iter()
                    .map(|annotation| {
                        format!("@{}={}", annotation.short_name, annotation.qualified_name)
                    })
                    .collect();
                out.push(format!(
                    "{} {} {}:{} [{}]",
                    kind(class),
                    class.simple_name,
                    class.qualified_name,
                    class.line,
                    annotations.join(" ")
                ));
                list(class.classes(), out);
            }
        }
        let mut listed = Vec::new();
        for file in &round.files {
            listed.push(format!("{} {}", file.path, file.package_name));
            list(file.classes(), &mut listed);
        }
        assert_eq!(
            listed,
            [
                "/src/Sample.kt sample",
                "ANNOTATION_CLASS Marker sample.Marker:3 []",
                "CLASS Plain sample.Plain:7 [@Marker=sample.Marker]",
                "INTERFACE Inner sample.Plain.Inner:8 []",
                "OBJECT Holder sample.Plain.Holder:11 [@Marker=sample.Marker]",
                "INTERFACE Shape sample.Shape:14 []",
                "OBJECT Single sample.Single:16 [@Marker=sample.Marker]",
                "ANNOTATION_CLASS Tag sample.Tag:18 []",
                "/src/Other.kt sample.other",
                "CLASS Root sample.other.Root:5 [@Marker=sample.Marker @Tag=sample.Tag]",
            ]
        );
    }

    #[test]
    fn members_and_their_types_are_listed_in_ksps_order() {
        let round = round(&[(
            "/src/Members.kt",
            "package members\n\ninterface Named {\n    val name: String\n    fun describe(prefix: String = \"\"): String\n}\n\nabstract class Base<T : Any>(protected val seed: T?) : Named {\n    abstract override val name: String\n    open fun seedOr(fallback: T): T = seed ?: fallback\n}\n\ndata class Item(override val name: String, var count: Int = 0) : Base<String>(null) {\n    private val tags: Array<String> = arrayOf()\n    internal var weight: Double? = null\n\n    constructor(name: String, tags: Array<String>) : this(name)\n\n    override fun describe(prefix: String): String = prefix + name\n\n    fun <R> mapTags(transform: (String) -> R): Array<R>? = null\n\n    suspend fun load(vararg ids: Long): Item? = null\n\n    companion object {\n        const val LIMIT: Int = 10\n        fun empty(): Item = Item(\"\")\n    }\n}\n\nfun topLevel(items: Array<out Item>, sink: Array<in String>, any: Array<*>): Unit {}\n\nval topValue: Array<Array<Int>> = arrayOf()\n\nval inferred = 1\n",
        )])
        .expect("every declaration is readable");
        assert_eq!(
            dumped(&round),
            [
                "fun topLevel kind=TOP_LEVEL abstract=false modifiers= typeParameters= receiver=null (items: kotlin.Array<out members.Item> vararg=false default=false, sink: kotlin.Array<in kotlin.String> vararg=false default=false, any: kotlin.Array<*> vararg=false default=false): kotlin.Unit",
                "property topValue mutable=false backingField=true modifiers=: kotlin.Array<kotlin.Array<kotlin.Int>>",
                "property inferred mutable=false backingField=true modifiers=: kotlin.Int",
                "INTERFACE members.Named modifiers= typeParameters= supertypes=kotlin.Any",
                "  property name mutable=false backingField=false modifiers=: kotlin.String",
                "  fun describe kind=MEMBER abstract=true modifiers= typeParameters= receiver=null (prefix: kotlin.String vararg=false default=true): kotlin.String",
                "CLASS members.Base modifiers=ABSTRACT typeParameters=T:kotlin.Any supertypes=members.Named",
                "  property seed mutable=false backingField=true modifiers=PROTECTED: members.Base.T?",
                "  property name mutable=false backingField=false modifiers=ABSTRACT,OVERRIDE: kotlin.String",
                "  fun seedOr kind=MEMBER abstract=false modifiers=OPEN typeParameters= receiver=null (fallback: members.Base.T vararg=false default=false): members.Base.T",
                "  fun <init> kind=MEMBER abstract=false modifiers=FINAL typeParameters=T receiver=null (seed: members.Base.T? vararg=false default=false): members.Base<*>",
                "CLASS members.Item modifiers=DATA typeParameters= supertypes=members.Base<kotlin.String>",
                "  property name mutable=false backingField=true modifiers=OVERRIDE: kotlin.String",
                "  property count mutable=true backingField=true modifiers=: kotlin.Int",
                "  property tags mutable=false backingField=true modifiers=PRIVATE: kotlin.Array<kotlin.String>",
                "  property weight mutable=true backingField=true modifiers=INTERNAL: kotlin.Double?",
                "  fun describe kind=MEMBER abstract=false modifiers=OVERRIDE typeParameters= receiver=null (prefix: kotlin.String vararg=false default=true): kotlin.String",
                "  fun mapTags kind=MEMBER abstract=false modifiers= typeParameters=R receiver=null (transform: kotlin.Function1<kotlin.String, members.Item.mapTags.R> vararg=false default=false): kotlin.Array<members.Item.mapTags.R>?",
                "  fun load kind=MEMBER abstract=false modifiers=SUSPEND typeParameters= receiver=null (ids: kotlin.Long vararg=true default=false): members.Item?",
                "  fun component1 kind=MEMBER abstract=false modifiers=FINAL,OPERATOR,OVERRIDE,PUBLIC typeParameters= receiver=null (): kotlin.String",
                "  fun component2 kind=MEMBER abstract=false modifiers=FINAL,OPERATOR,OVERRIDE,PUBLIC typeParameters= receiver=null (): kotlin.Int",
                "  fun copy kind=MEMBER abstract=false modifiers=FINAL,PUBLIC typeParameters= receiver=null (name: kotlin.String vararg=false default=true, count: kotlin.Int vararg=false default=true): members.Item",
                "  fun <init> kind=MEMBER abstract=false modifiers=FINAL typeParameters= receiver=null (name: kotlin.String vararg=false default=false, count: kotlin.Int vararg=false default=true): members.Item",
                "  fun <init> kind=MEMBER abstract=false modifiers=FINAL typeParameters= receiver=null (name: kotlin.String vararg=false default=false, tags: kotlin.Array<kotlin.String> vararg=false default=false): members.Item",
                "  OBJECT members.Item.Companion modifiers= typeParameters= supertypes=kotlin.Any",
                "    property LIMIT mutable=false backingField=true modifiers=CONST: kotlin.Int",
                "    fun empty kind=MEMBER abstract=false modifiers= typeParameters= receiver=null (): members.Item",
                "    fun <init> kind=MEMBER abstract=false modifiers=FINAL,PUBLIC typeParameters= receiver=null (): members.Item.Companion",
            ]
        );
    }

    #[test]
    fn a_vararg_property_has_its_array_and_an_external_member_is_not_abstract() {
        let round = round(&[(
            "/src/Members.kt",
            "package members\n\nclass Packed(vararg val words: String)\n\nclass Numbers(vararg val ids: Int)\n\nclass NativeApi {\n    external fun ping(): Int\n}\n",
        )])
        .expect("every declaration is readable");
        assert_eq!(
            dumped(&round),
            [
                "CLASS members.Packed modifiers= typeParameters= supertypes=kotlin.Any",
                "  property words mutable=false backingField=true modifiers=VARARG: kotlin.Array<out kotlin.String>",
                "  fun <init> kind=MEMBER abstract=false modifiers=FINAL typeParameters= receiver=null (words: kotlin.String vararg=true default=false): members.Packed",
                "CLASS members.Numbers modifiers= typeParameters= supertypes=kotlin.Any",
                "  property ids mutable=false backingField=true modifiers=VARARG: kotlin.IntArray",
                "  fun <init> kind=MEMBER abstract=false modifiers=FINAL typeParameters= receiver=null (ids: kotlin.Int vararg=true default=false): members.Numbers",
                "CLASS members.NativeApi modifiers= typeParameters= supertypes=kotlin.Any",
                "  fun ping kind=MEMBER abstract=false modifiers=EXTERNAL typeParameters= receiver=null (): kotlin.Int",
                "  fun <init> kind=MEMBER abstract=false modifiers=FINAL,PUBLIC typeParameters= receiver=null (): members.NativeApi",
            ]
        );
    }

    #[test]
    fn declarations_are_in_ksps_order_with_the_members_the_language_contributes() {
        let order = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/krusty-plugin-host/tests/fixtures/ksp/order/Order.kt"
        ))
        .unwrap();
        let expected: Vec<String> = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/krusty-plugin-host/tests/fixtures/ksp/order.expected"
        ))
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("w: [ksp] "))
        .map(str::to_string)
        .collect();
        let round = round(&[("/src/Order.kt", &order)]).expect("every declaration is readable");
        assert_eq!(dumped(&round), expected);
    }

    #[test]
    fn a_primary_constructor_is_the_resolved_declarations_with_its_written_parameters() {
        let round = round(&[(
            "/src/Constructors.kt",
            "package constructors\n\nclass Implicit\n\nclass Explicit(first: Int, second: String = \"\")\n\nclass SecondaryOnly {\n    constructor(value: Int)\n}\n\nclass Both(val a: Int, var b: String = \"\", vararg c: Int) {\n    constructor() : this(1)\n}\n\nobject Single\n\nannotation class Marker(val name: String)\n\ninterface Shape\n\nclass Outer {\n    inner class In(val x: Int)\n}\n",
        )])
        .expect("every declaration is readable");
        let file = &round.files[0];
        let outer = file.classes().nth(7).expect("Outer");
        let constructors: Vec<String> = file
            .classes()
            .chain(outer.classes())
            .map(|class| {
                let primary = class.primary_constructor().map(|constructor| {
                    let parameters: Vec<String> = constructor
                        .parameters
                        .iter()
                        .map(|parameter| {
                            let property = match (parameter.is_val, parameter.is_var) {
                                (true, _) => "val ",
                                (_, true) => "var ",
                                _ => "",
                            };
                            format!("{property}{}", self::parameter(parameter))
                        })
                        .collect();
                    parameters.join(", ")
                });
                format!("{} {primary:?}", class.qualified_name)
            })
            .collect();
        assert_eq!(
            constructors,
            [
                "constructors.Implicit Some(\"\")",
                "constructors.Explicit Some(\"first: kotlin.Int vararg=false default=false, second: kotlin.String vararg=false default=true\")",
                "constructors.SecondaryOnly None",
                "constructors.Both Some(\"val a: kotlin.Int vararg=false default=false, var b: kotlin.String vararg=false default=true, c: kotlin.Int vararg=true default=false\")",
                "constructors.Single Some(\"\")",
                "constructors.Marker Some(\"val name: kotlin.String vararg=false default=false\")",
                "constructors.Shape None",
                "constructors.Outer Some(\"\")",
                "constructors.Outer.In Some(\"val x: kotlin.Int vararg=false default=false\")",
            ]
        );
    }

    #[test]
    fn a_companion_object_is_named_by_its_resolved_identity() {
        let round = round(&[(
            "/src/Companions.kt",
            "package p\n\nclass Outer {\n    companion object\n}\n\nclass Named {\n    companion object Factory {\n        object Deep\n    }\n}\n",
        )])
        .expect("every declaration is readable");
        fn names<'a>(classes: impl Iterator<Item = &'a KspClass>, out: &mut Vec<(String, String)>) {
            for class in classes {
                out.push((class.simple_name.clone(), class.qualified_name.clone()));
                names(class.classes(), out);
            }
        }
        let mut listed = Vec::new();
        names(round.files[0].classes(), &mut listed);
        assert_eq!(
            listed,
            [
                ("Outer", "p.Outer"),
                ("Companion", "p.Outer.Companion"),
                ("Named", "p.Named"),
                ("Factory", "p.Named.Factory"),
                ("Deep", "p.Named.Factory.Deep"),
            ]
            .map(|(simple, qualified)| (simple.to_string(), qualified.to_string()))
        );
    }

    #[test]
    fn an_enum_entry_with_a_body_says_its_list_is_incomplete() {
        let round = round(&[(
            "/src/Entries.kt",
            "package entries\n\nenum class Choice {\n    First {\n        class Nested\n    },\n    Second {\n        fun f() {}\n    },\n    Third\n}\n",
        )])
        .expect("every declaration is readable");
        let choice = round.files[0].classes().next().expect("Choice");
        assert!(choice.declarations_complete);
        let entries: Vec<(&str, bool)> = choice
            .classes()
            .map(|entry| (entry.qualified_name.as_str(), entry.declarations_complete))
            .collect();
        assert_eq!(
            entries,
            [
                ("entries.Choice.First", false),
                ("entries.Choice.Second", false),
                ("entries.Choice.Third", true),
            ]
        );
    }

    #[test]
    fn an_inline_member_leaves_its_classifiers_members_resolved() {
        let round = round(&[(
            "/src/Members.kt",
            "package members\n\nclass Holder {\n    inline fun twice(x: Int): Int = x * 2\n    fun named(label: String): String = label\n}\n",
        )])
        .expect("every declaration is readable");
        assert_eq!(
            dumped(&round),
            [
                "CLASS members.Holder modifiers= typeParameters= supertypes=kotlin.Any",
                "  fun twice kind=MEMBER abstract=false modifiers=INLINE typeParameters= receiver=null (x: kotlin.Int vararg=false default=false): kotlin.Int",
                "  fun named kind=MEMBER abstract=false modifiers= typeParameters= receiver=null (label: kotlin.String vararg=false default=false): kotlin.String",
                "  fun <init> kind=MEMBER abstract=false modifiers=FINAL,PUBLIC typeParameters= receiver=null (): members.Holder",
            ]
        );
    }

    #[test]
    fn an_unresolved_annotation_is_refused_where_it_is_applied() {
        assert_eq!(
            round(&[("/src/Missing.kt", "package missing\n\n@Absent\nclass Annotated\n")]),
            Err("/src/Missing.kt:4: KSP cannot read class Annotated: it has the annotation @Absent, which did not resolve".to_string())
        );
        assert_eq!(
            round(&[("/src/Missing.kt", "package missing\n\nclass Holder {\n    @Absent\n    fun annotated() {}\n}\n")]),
            Err("/src/Missing.kt:5: KSP cannot read function annotated: it has the annotation @Absent, which did not resolve".to_string())
        );
    }

    #[test]
    fn a_header_has_its_written_modifiers_type_parameters_and_supertypes() {
        let round = round(&[(
            "/src/Headers.kt",
            "package headers\n\ninterface Named\n\ninterface Box<in V>\n\nabstract class Base<T : Any, in I> : Named\n\nopen class Middle<out E : Named?> : Base<Array<E>, String>(), Box<Middle<*>>\n\nsealed interface Shape<S> : Named where S : Named, S : Box<S>\n\nprivate class Hidden : Shape<Hidden>, Box<Hidden>\n\ninternal annotation class Marker\n\nenum class Color { RED }\n\nclass Outer {\n    inner class In<X> : Named, Box<X?>\n\n    data object Single\n\n    companion object\n}\n\nfun interface Action : () -> Unit\n",
        )])
        .expect("every declaration is readable");
        let outer = round.files[0].classes().nth(8).expect("Outer");
        let headers: Vec<String> = round.files[0]
            .classes()
            .chain(outer.classes())
            .map(class_line)
            .collect();
        assert_eq!(
            headers,
            [
                "INTERFACE headers.Named modifiers= typeParameters= supertypes=kotlin.Any",
                "INTERFACE headers.Box modifiers= typeParameters=V:kotlin.Any? supertypes=kotlin.Any",
                "CLASS headers.Base modifiers=ABSTRACT typeParameters=T:kotlin.Any,I:kotlin.Any? supertypes=headers.Named",
                "CLASS headers.Middle modifiers=OPEN typeParameters=E:headers.Named? supertypes=headers.Base<kotlin.Array<headers.Middle.E>, kotlin.String>,headers.Box<headers.Middle<*>>",
                "INTERFACE headers.Shape modifiers=SEALED typeParameters=S:headers.Named&headers.Box<headers.Shape.S> supertypes=headers.Named",
                "CLASS headers.Hidden modifiers=PRIVATE typeParameters= supertypes=headers.Shape<headers.Hidden>,headers.Box<headers.Hidden>",
                "ANNOTATION_CLASS headers.Marker modifiers=ANNOTATION,INTERNAL typeParameters= supertypes=kotlin.Annotation",
                "ENUM_CLASS headers.Color modifiers=ENUM typeParameters= supertypes=kotlin.Enum<headers.Color>",
                "CLASS headers.Outer modifiers= typeParameters= supertypes=kotlin.Any",
                "INTERFACE headers.Action modifiers=FUN typeParameters= supertypes=kotlin.Function0<kotlin.Unit>",
                "CLASS headers.Outer.In modifiers=INNER typeParameters=X:kotlin.Any? supertypes=headers.Named,headers.Box<headers.Outer.In.X?>",
                "OBJECT headers.Outer.Single modifiers=DATA typeParameters= supertypes=kotlin.Any",
                "OBJECT headers.Outer.Companion modifiers= typeParameters= supertypes=kotlin.Any",
            ]
        );
    }
}
