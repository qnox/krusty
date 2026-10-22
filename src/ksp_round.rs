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

use krusty_plugin_ir::{KspAnnotation, KspClass, KspClassKind, KspFile, KspParameter, KspRound};

use crate::ast::{AnnotationRef, ClassDecl, ClassKind, Decl, DeclId, File};
use crate::frontend::{FrontendClassSig, FrontendSymbols, SourceSetAnalysis};
use crate::types::TypeName;

/// The round a processor reads from `analysis`, whose files are at `paths` (one per source, in the
/// analysis's order). `analysis` collects every file's signatures and checks no body
/// (`analyze_source_set_prefix_with_features(sources, 0, sources.len(), …)`). A declaration the round cannot name, such as an annotation that did not
/// resolve, is refused by what it is and where.
pub fn ksp_round(analysis: &SourceSetAnalysis, paths: &[String]) -> Result<KspRound, String> {
    if paths.len() != analysis.files.len() {
        return Err(format!(
            "KSP needs the path of every source: {} paths were given for {} sources",
            paths.len(),
            analysis.files.len()
        ));
    }
    let declared: crate::fx_hash::HashMap<(u32, DeclId), &FrontendClassSig> = analysis
        .symbols
        .classes
        .values()
        .filter_map(|signature| Some(((signature.source_file, signature.source_decl?), signature)))
        .collect();
    let mut files = Vec::with_capacity(paths.len());
    for (index, (file, path)) in analysis.files.iter().zip(paths).enumerate() {
        let source = Source {
            index: index as u32,
            file,
            path,
            symbols: &analysis.symbols,
            declared: &declared,
        };
        files.push(source.file()?);
    }
    Ok(KspRound { files })
}

/// One analyzed source file, and what its classifiers resolved to.
struct Source<'a> {
    index: u32,
    file: &'a File,
    path: &'a str,
    symbols: &'a FrontendSymbols,
    /// The resolved signature of each source classifier, by file and declaration.
    declared: &'a crate::fx_hash::HashMap<(u32, DeclId), &'a FrontendClassSig>,
}

impl Source<'_> {
    fn file(&self) -> Result<KspFile, String> {
        let package_name = self.file.package.clone().unwrap_or_default();
        let mut classes = Vec::new();
        let mut declarations_complete = self.file.type_alias_decls.is_empty();
        for &id in &self.file.decls {
            if !self.is_member_of(id, None) {
                continue;
            }
            match self.file.decl(id) {
                Decl::Class(class) => {
                    classes.push(self.class(id, class, &package_name, None)?);
                }
                Decl::Fun(_) | Decl::Property(_) => declarations_complete = false,
            }
        }
        Ok(KspFile {
            path: self.path.to_string(),
            package_name,
            classes,
            declarations_complete,
        })
    }

    /// Whether `id` is declared directly in `owner`'s body, or at the top level when `owner` is
    /// `None`. Local and anonymous classifiers are in neither: KSP does not list them. Nor is a
    /// classifier in an enum entry's body, which the round does not describe: its enum class says
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
        package_name: &str,
        owner: Option<&str>,
    ) -> Result<KspClass, String> {
        let simple_name = self
            .file
            .hoisted_classifier_source_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| class.name.clone());
        let qualified_name = match (owner, package_name) {
            (Some(owner), _) => format!("{owner}.{simple_name}"),
            (None, "") => simple_name.clone(),
            (None, package) => format!("{package}.{simple_name}"),
        };
        let kind = match class.kind {
            ClassKind::Interface => KspClassKind::Interface,
            ClassKind::Enum => KspClassKind::EnumClass,
            ClassKind::Annotation => KspClassKind::AnnotationClass,
            ClassKind::Class if class.singleton => KspClassKind::Object,
            ClassKind::Class => KspClassKind::Class,
        };
        let Some(signature) = self.declared.get(&(self.index, id)) else {
            return Err(self.refusal(class, "has no resolved declaration"));
        };
        let mut annotations = Vec::with_capacity(class.annotations.len());
        for annotation in &class.annotations {
            annotations.push(self.annotation(class, annotation)?);
        }
        let mut classes = Vec::new();
        for &nested in &self.file.decls {
            if !self.is_member_of(nested, Some(id)) {
                continue;
            }
            if let Decl::Class(nested_class) = self.file.decl(nested) {
                classes.push(self.class(
                    nested,
                    nested_class,
                    package_name,
                    Some(&qualified_name),
                )?);
            }
        }
        let primary_constructor = self.primary_constructor(class, kind, signature)?;
        let declarations_complete = kind != KspClassKind::EnumClass
            && class.props.is_empty()
            && class.methods.is_empty()
            && class.body_props.is_empty()
            && class.secondary_ctors.is_empty()
            && class.type_aliases.is_empty()
            && class.companion_block_members.is_empty();
        Ok(KspClass {
            simple_name,
            qualified_name,
            kind,
            line: class.decl_line,
            annotations,
            primary_constructor,
            classes,
            declarations_complete,
        })
    }

    /// The parameters of the primary constructor the resolved declaration has: the one written, or
    /// the implicit one of a class without secondary constructors. An interface has no constructor.
    fn primary_constructor(
        &self,
        class: &ClassDecl,
        kind: KspClassKind,
        signature: &FrontendClassSig,
    ) -> Result<Option<Vec<KspParameter>>, String> {
        if kind == KspClassKind::Interface || !signature.has_primary_ctor {
            return Ok(None);
        }
        // The signature names each parameter and whether it has a default; the declaration says
        // how each was written.
        if signature.ctor_param_names.len() != class.props.len() {
            return Err(self.refusal(
                class,
                &format!(
                    "has {} primary-constructor parameters resolved for {} written",
                    signature.ctor_param_names.len(),
                    class.props.len()
                ),
            ));
        }
        let parameters = signature
            .ctor_param_names
            .iter()
            .zip(&class.props)
            .map(|((name, has_default), written)| KspParameter {
                name: name.clone(),
                is_val: written.is_property && !written.is_var,
                is_var: written.is_property && written.is_var,
                is_vararg: written.is_vararg,
                has_default: *has_default,
            })
            .collect();
        Ok(Some(parameters))
    }

    fn annotation(
        &self,
        class: &ClassDecl,
        annotation: &AnnotationRef,
    ) -> Result<KspAnnotation, String> {
        let Some(resolved) = self.symbols.resolved_annotation(self.index, annotation) else {
            return Err(self.refusal(
                class,
                &format!(
                    "has the annotation @{}, which did not resolve",
                    annotation.name
                ),
            ));
        };
        // KSP's short name is the name as written, without a qualifier.
        let short_name = annotation
            .name
            .rsplit('.')
            .next()
            .unwrap_or(&annotation.name)
            .to_string();
        Ok(KspAnnotation {
            short_name,
            qualified_name: qualified_name(resolved),
        })
    }

    fn refusal(&self, class: &ClassDecl, what: &str) -> String {
        format!(
            "{}:{}: KSP cannot read class {}: it {what}",
            self.path, class.decl_line, class.name
        )
    }
}

/// KSP's dot-separated qualified name of a classifier: its package, then each enclosing classifier
/// and itself.
fn qualified_name(class: TypeName) -> String {
    match class.nested_owner() {
        Some(owner) => format!("{}.{}", qualified_name(owner), class.nested_segment_ref()),
        None => {
            let package = class.package().replace('/', ".");
            let segment = class.segment();
            if package.is_empty() {
                segment
            } else {
                format!("{package}.{segment}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::features::LangFeatures;
    use crate::frontend::{analyze_source_set_prefix_with_features, PlatformProvider, SourceInput};
    use crate::libraries::EmptySymbolSource;

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
        ksp_round(&analysis, &paths)
    }

    fn class(
        simple_name: &str,
        qualified_name: &str,
        kind: KspClassKind,
        line: u32,
        annotations: &[(&str, &str)],
        classes: Vec<KspClass>,
    ) -> KspClass {
        KspClass {
            simple_name: simple_name.to_string(),
            qualified_name: qualified_name.to_string(),
            kind,
            line,
            annotations: annotations
                .iter()
                .map(|(short_name, qualified_name)| KspAnnotation {
                    short_name: short_name.to_string(),
                    qualified_name: qualified_name.to_string(),
                })
                .collect(),
            primary_constructor: (kind != KspClassKind::Interface).then(Vec::new),
            classes,
            declarations_complete: true,
        }
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
        let marker = [("Marker", "sample.Marker")];
        assert_eq!(
            round,
            KspRound {
                files: vec![
                    KspFile {
                        path: "/src/Sample.kt".to_string(),
                        package_name: "sample".to_string(),
                        classes: vec![
                            class(
                                "Marker",
                                "sample.Marker",
                                KspClassKind::AnnotationClass,
                                3,
                                &[],
                                vec![]
                            ),
                            class(
                                "Plain",
                                "sample.Plain",
                                KspClassKind::Class,
                                7,
                                &marker,
                                vec![
                                    class(
                                        "Inner",
                                        "sample.Plain.Inner",
                                        KspClassKind::Interface,
                                        8,
                                        &[],
                                        vec![]
                                    ),
                                    class(
                                        "Holder",
                                        "sample.Plain.Holder",
                                        KspClassKind::Object,
                                        11,
                                        &marker,
                                        vec![]
                                    ),
                                ],
                            ),
                            class(
                                "Shape",
                                "sample.Shape",
                                KspClassKind::Interface,
                                14,
                                &[],
                                vec![]
                            ),
                            class(
                                "Single",
                                "sample.Single",
                                KspClassKind::Object,
                                16,
                                &marker,
                                vec![]
                            ),
                            class(
                                "Tag",
                                "sample.Tag",
                                KspClassKind::AnnotationClass,
                                18,
                                &[],
                                vec![]
                            ),
                        ],
                        declarations_complete: true,
                    },
                    KspFile {
                        path: "/src/Other.kt".to_string(),
                        package_name: "sample.other".to_string(),
                        classes: vec![class(
                            "Root",
                            "sample.other.Root",
                            KspClassKind::Class,
                            5,
                            &[("Marker", "sample.Marker"), ("Tag", "sample.Tag")],
                            vec![],
                        )],
                        declarations_complete: true,
                    },
                ],
            }
        );
    }

    #[test]
    fn a_body_or_file_with_other_declarations_says_its_list_is_incomplete() {
        let round = round(&[(
            "/src/Members.kt",
            "package members\n\nclass Holder {\n    fun member() {}\n}\n\nfun topLevel() {}\n",
        )])
        .expect("every declaration is readable");
        let file = &round.files[0];
        assert!(!file.declarations_complete);
        assert!(!file.classes[0].declarations_complete);
    }

    fn parameter(name: &str, flags: (bool, bool, bool, bool)) -> KspParameter {
        let (is_val, is_var, is_vararg, has_default) = flags;
        KspParameter {
            name: name.to_string(),
            is_val,
            is_var,
            is_vararg,
            has_default,
        }
    }

    #[test]
    fn a_primary_constructor_is_the_resolved_declarations_with_its_written_parameters() {
        let round = round(&[(
            "/src/Constructors.kt",
            "package constructors\n\nclass Implicit\n\nclass Explicit(first: Int, second: String = \"\")\n\nclass SecondaryOnly {\n    constructor(value: Int)\n}\n\nclass Both(val a: Int, var b: String = \"\", vararg c: Int) {\n    constructor() : this(1)\n}\n\nobject Single\n\nannotation class Marker(val name: String)\n\ninterface Shape\n\nclass Outer {\n    inner class In(val x: Int)\n}\n",
        )])
        .expect("every declaration is readable");
        let constructors: Vec<(&str, Option<Vec<KspParameter>>)> = round.files[0]
            .classes
            .iter()
            .chain(&round.files[0].classes[7].classes)
            .map(|class| {
                (
                    class.qualified_name.as_str(),
                    class.primary_constructor.clone(),
                )
            })
            .collect();
        let plain = (false, false, false, false);
        assert_eq!(
            constructors,
            [
                ("constructors.Implicit", Some(vec![])),
                (
                    "constructors.Explicit",
                    Some(vec![
                        parameter("first", plain),
                        parameter("second", (false, false, false, true)),
                    ])
                ),
                ("constructors.SecondaryOnly", None),
                (
                    "constructors.Both",
                    Some(vec![
                        parameter("a", (true, false, false, false)),
                        parameter("b", (false, true, false, true)),
                        parameter("c", (false, false, true, false)),
                    ])
                ),
                ("constructors.Single", Some(vec![])),
                (
                    "constructors.Marker",
                    Some(vec![parameter("name", (true, false, false, false))])
                ),
                ("constructors.Shape", None),
                ("constructors.Outer", Some(vec![])),
                (
                    "constructors.Outer.In",
                    Some(vec![parameter("x", (true, false, false, false))])
                ),
            ]
        );
    }

    #[test]
    fn a_classifier_in_an_enum_entry_body_is_not_listed_in_its_file() {
        let round = round(&[(
            "/src/Entries.kt",
            "package entries\n\nenum class Choice {\n    First {\n        class Nested\n    }\n}\n",
        )])
        .expect("every declaration is readable");
        let file = &round.files[0];
        let names: Vec<&str> = file
            .classes
            .iter()
            .map(|class| class.qualified_name.as_str())
            .collect();
        assert_eq!(names, ["entries.Choice"]);
        assert!(file.classes[0].classes.is_empty());
        assert!(!file.classes[0].declarations_complete);
    }

    #[test]
    fn an_unresolved_annotation_is_refused_where_it_is_applied() {
        assert_eq!(
            round(&[("/src/Missing.kt", "package missing\n\n@Absent\nclass Annotated\n")]),
            Err("/src/Missing.kt:4: KSP cannot read class Annotated: it has the annotation @Absent, which did not resolve".to_string())
        );
    }
}
