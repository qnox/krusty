//! Annotations as KSP shows them: the checked applications the frontend published for each
//! declaration, with their arguments in the order the source wrote them and the defaults of the
//! elements they did not write.

use krusty_plugin_ir::{KspAnnotation, KspAnnotationArguments, KspArgument, KspUseSite, KspValue};

use super::type_model::{qualified_name, TypeParameterScope};
use super::{Source, Subject};
use crate::ast::AnnotationRef;
use crate::frontend::{FrontendSymbols, SourceSetAnalysis};
use crate::libraries::DeclarationLanguage;
use crate::types::{
    stored_value_ty, AnnotationValue, PropertyAnnotationSite, ResolvedAnnotation, Ty, TyKind,
    TypeName,
};

/// Which of the applications written on a declaration KSP shows on one of the nodes it makes of
/// it. A primary constructor's `val` parameter is a parameter and a property, and an annotation
/// without a use-site target is on both.
#[derive(Clone, Copy)]
pub(super) enum Placement {
    /// Every application on a classifier, function or constructor, which KSP names as the source
    /// wrote it: by an import alias's name when the source used one.
    Declaration,
    /// Every application on an accessor or a parameter, which KSP names by its class's name.
    All,
    /// A property: the unprefixed ones and those targeting the property or its field.
    Property,
    /// A constructor parameter: the unprefixed ones and those targeting the parameter.
    Parameter,
    /// A setter's parameter: those targeting it from the property (`@setparam:`), which KSP
    /// reports as the compiler's.
    SetterParameter,
}

impl Placement {
    fn shows(self, use_site: Option<PropertyAnnotationSite>) -> bool {
        use PropertyAnnotationSite as Site;
        match self {
            Placement::Declaration | Placement::All => true,
            Placement::Property => matches!(use_site, None | Some(Site::Property | Site::Field)),
            Placement::Parameter => matches!(use_site, None | Some(Site::ValueParameter)),
            Placement::SetterParameter => use_site == Some(Site::SetterParameter),
        }
    }
}

impl Source<'_> {
    /// The applications of `published`, those of the site the source wrote `written` on, that
    /// `placement` shows, once every one of `written` has resolved and been published. `None`
    /// when the round cannot identify the site, which is refused when anything is written there.
    pub(super) fn published_annotations<'a>(
        &self,
        subject: Subject<'_>,
        written: impl IntoIterator<Item = &'a AnnotationRef>,
        published: Option<&[ResolvedAnnotation]>,
        placement: Placement,
    ) -> Result<Vec<KspAnnotation>, String> {
        let written = written.into_iter().collect::<Vec<_>>();
        for annotation in &written {
            self.annotation(subject, annotation)?;
        }
        let Some(published) = published else {
            if written.is_empty() {
                return Ok(Vec::new());
            }
            return Err(self.refusal(
                subject,
                "has annotations on a declaration it cannot identify",
            ));
        };
        if written.len() != published.len() {
            return Err(self.refusal(
                subject,
                "has an annotation whose arguments are not compile-time constants",
            ));
        }
        Ok(published
            .iter()
            .filter(|annotation| placement.shows(use_site(annotation)))
            .map(|annotation| {
                Annotations::new(self.analysis).ksp_annotation(
                    annotation,
                    matches!(placement, Placement::SetterParameter),
                    true,
                    matches!(placement, Placement::Declaration),
                )
            })
            .collect())
    }
}

/// What KSP shows of resolved annotation applications, from the analysis they were resolved in:
/// their classes' element defaults and the classifiers their values name.
pub(super) struct Annotations<'a> {
    analysis: &'a SourceSetAnalysis,
    symbols: &'a FrontendSymbols,
}

impl<'a> Annotations<'a> {
    pub(super) fn new(analysis: &'a SourceSetAnalysis) -> Self {
        Annotations {
            analysis,
            symbols: &analysis.symbols,
        }
    }

    /// `annotation` as KSP shows it. `written` says whether the source wrote it or it is a
    /// default; a default's arguments are all defaults too. `as_written` names it as the source
    /// wrote it rather than by its class's name.
    pub(super) fn ksp_annotation(
        &self,
        annotation: &ResolvedAnnotation,
        synthetic: bool,
        written: bool,
        as_written: bool,
    ) -> KspAnnotation {
        let short_name = match &annotation.written {
            Some(form) if as_written => form.short_name.to_string(),
            _ => short_name(annotation.annotation),
        };
        KspAnnotation {
            short_name,
            qualified_name: qualified_name(annotation.annotation),
            use_site: use_site(annotation).map(ksp_use_site),
            synthetic,
            arguments: self.arguments(annotation, written),
        }
    }

    /// The arguments of `annotation` and its class's defaults, or `None` when the round has
    /// no record of the defaults or cannot describe a value.
    fn arguments(
        &self,
        annotation: &ResolvedAnnotation,
        written: bool,
    ) -> Option<KspAnnotationArguments> {
        let (defaults_read, defaults) = self.element_defaults(annotation.annotation)?;
        let defaults = defaults
            .iter()
            .map(|(name, value)| {
                Some(KspArgument {
                    name: name.clone(),
                    value: self.value(value, false, defaults_read)?,
                    written: false,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let read = match annotation.written {
            Some(_) => ValueSource::Source,
            None => ValueSource::ClassFile,
        };
        // An annotation read from a class file records no written order: every value it has is
        // listed, in declaration order.
        let order = match &annotation.written {
            Some(form) => form
                .order
                .iter()
                .map(|&position| usize::from(position))
                .collect(),
            None => (0..annotation.arguments.len()).collect::<Vec<_>>(),
        };
        let mut arguments = order
            .into_iter()
            .map(|position| {
                let (name, value) = annotation.arguments.get(position)?;
                Some(KspArgument {
                    name: name.clone(),
                    value: self.value(value, written, read)?,
                    written,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        arguments.extend(
            defaults
                .iter()
                .filter(|default| {
                    arguments
                        .iter()
                        .all(|argument| argument.name != default.name)
                })
                .cloned()
                .collect::<Vec<_>>(),
        );
        Some(KspAnnotationArguments {
            arguments,
            defaults,
        })
    }

    /// The defaults of the elements of the annotation class `class` in declaration order, and
    /// where they were read: the published ones of the module's own, or those a library's
    /// declaration carries. `None` when one of them is not known.
    fn element_defaults(
        &self,
        class: TypeName,
    ) -> Option<(ValueSource, Vec<(String, AnnotationValue)>)> {
        if let Some(source) = self.symbols.classes.get(&class) {
            // Pass 1 publishes the folded defaults of a class that declares any.
            if !source
                .ctor_param_names
                .iter()
                .any(|(_, has_default)| *has_default)
            {
                return Some((ValueSource::Source, Vec::new()));
            }
            return self
                .analysis
                .annotation_element_defaults(source.stable_declaration?)
                .map(|defaults| (ValueSource::Source, defaults.to_vec()));
        }
        // A library class has the element defaults its class file records. A Kotlin `vararg val`
        // element declared without one materializes an omitted empty array, and KSP2 lists that
        // array as its default; it lists an empty array as the default of every array element a
        // Java annotation declares without one too.
        let library = self.symbols.libraries.classifier(class)?;
        let java =
            self.symbols.libraries.declaration_header(class)?.language == DeclarationLanguage::Java;
        let application = library.annotation_application()?;
        let parameters = &application.parameters;
        let defaults = parameters
            .names
            .iter()
            .zip(&parameters.types)
            .enumerate()
            .filter_map(
                |(index, (name, ty))| match library.annotation_element_default(name) {
                    Some(default) => {
                        Some(default.value().map(|value| (name.clone(), value.clone())))
                    }
                    None => ((application.policy.materialize_omitted_vararg
                        && parameters.vararg == Some(index))
                        || (java && ty.array_elem().is_some()))
                    .then(|| Some((name.clone(), AnnotationValue::Array(Vec::new())))),
                },
            )
            .collect::<Option<_>>()?;
        Some((ValueSource::ClassFile, defaults))
    }

    /// The type a class literal denotes, as KSP describes it. One written in source is the
    /// classifier applied to a star for each of its type parameters; its value records no array's
    /// element type, so an array class literal is not described. One read from a class file names
    /// the JVM class it records as it is read in Kotlin, with no type arguments but an array's
    /// element.
    fn class_literal(&self, ty: Ty, read: ValueSource) -> Option<krusty_plugin_ir::KspType> {
        let scope = TypeParameterScope::default();
        if read == ValueSource::ClassFile {
            return scope.ty(self.read_as_kotlin(ty));
        }
        let class = stored_value_ty(ty).kotlin_class_internal()?;
        if class == crate::types::type_name("kotlin/Array") {
            return None;
        }
        let arity = match self.symbols.classes.get(&class) {
            Some(source) => source.type_params().len(),
            None => self
                .symbols
                .libraries
                .classifier(class)?
                .type_params()
                .len(),
        };
        let mut described = scope.ty(ty)?;
        if described.arguments.is_empty() {
            described.arguments = vec![krusty_plugin_ir::KspTypeArgument::Star; arity];
        }
        Some(described)
    }

    /// The class `ty` a class file records as Kotlin reads it: a platform class the platform maps
    /// to a Kotlin classifier (`java.lang.Integer` to `kotlin.Int`) is that classifier, and so is an
    /// array's element.
    fn read_as_kotlin(&self, ty: Ty) -> Ty {
        let TyKind::Obj(class, arguments) = ty.kind() else {
            return ty;
        };
        if class == crate::types::wk::array() {
            let arguments: Vec<Ty> = arguments
                .as_slice()
                .iter()
                .map(|argument| self.read_as_kotlin(*argument))
                .collect();
            return Ty::obj_args_name(class, &arguments);
        }
        match self.symbols.libraries.mapped_classifier(class) {
            Some(mapped) => Ty::obj_name(mapped).canonical_semantic(),
            None => ty,
        }
    }

    /// `value`, read from where `read` says, as KSP hands it over; a nested annotation is written
    /// when `written` says the argument holding it is.
    fn value(&self, value: &AnnotationValue, written: bool, read: ValueSource) -> Option<KspValue> {
        Some(match value {
            AnnotationValue::Int(value) => KspValue::Int(*value),
            AnnotationValue::Byte(value) => KspValue::Byte(*value),
            AnnotationValue::Short(value) => KspValue::Short(*value),
            AnnotationValue::Long(value) => KspValue::Long(*value),
            AnnotationValue::UByte(value) => KspValue::UByte(*value),
            AnnotationValue::UShort(value) => KspValue::UShort(*value),
            AnnotationValue::UInt(value) => KspValue::UInt(*value),
            AnnotationValue::ULong(value) => KspValue::ULong(*value),
            AnnotationValue::Float(value) => KspValue::Float(value.to_bits()),
            AnnotationValue::Double(value) => KspValue::Double(value.to_bits()),
            AnnotationValue::Boolean(value) => KspValue::Boolean(*value),
            AnnotationValue::Char(value) => KspValue::Char(*value),
            // A string holding a lone surrogate has no UTF-8 form to send.
            AnnotationValue::String(value) => KspValue::String(value.as_str()?.to_string()),
            AnnotationValue::Enum(class, entry) => KspValue::EnumEntry {
                class: qualified_name(*class),
                entry: entry.clone(),
            },
            AnnotationValue::Class(ty) => KspValue::Type(self.class_literal(*ty, read)?),
            AnnotationValue::Annotation {
                internal,
                values,
                written: form,
            } => {
                let nested = ResolvedAnnotation {
                    annotation: *internal,
                    arguments: values.clone(),
                    facts: Default::default(),
                    written: form.clone(),
                };
                KspValue::Annotation(Box::new(
                    self.ksp_annotation(&nested, !written, written, false),
                ))
            }
            AnnotationValue::Array(values) => KspValue::Array(
                values
                    .iter()
                    .map(|value| self.value(value, written, read))
                    .collect::<Option<_>>()?,
            ),
        })
    }
}

/// Where an annotation value was read: folded from source, or from a class file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueSource {
    Source,
    ClassFile,
}

fn use_site(annotation: &ResolvedAnnotation) -> Option<PropertyAnnotationSite> {
    annotation.written.as_ref()?.use_site
}

fn ksp_use_site(site: PropertyAnnotationSite) -> KspUseSite {
    match site {
        PropertyAnnotationSite::ValueParameter => KspUseSite::Param,
        PropertyAnnotationSite::Property => KspUseSite::Property,
        PropertyAnnotationSite::Field => KspUseSite::Field,
        PropertyAnnotationSite::Getter => KspUseSite::Get,
        PropertyAnnotationSite::Setter => KspUseSite::Set,
        PropertyAnnotationSite::SetterParameter => KspUseSite::SetParam,
    }
}

/// KSP's short name of an annotation: its class's simple name.
pub(super) fn short_name(class: TypeName) -> String {
    match class.nested_owner() {
        Some(_) => class.nested_segment_ref().to_string(),
        None => class.segment(),
    }
}
