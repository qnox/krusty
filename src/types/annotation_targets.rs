//! Where an annotation class may be applied: its declared Kotlin targets.
//!
//! The applicable set decides two things. A checker rejects an application on a declaration whose
//! target the set does not list (kotlinc's WRONG_ANNOTATION_TARGET), naming the set in its message.
//! And an application written on a property declaration without a use-site prefix lands on the
//! first applicable of `param` → `property` → `field`, three different places in a class file.

/// A Kotlin annotation target (`kotlin.annotation.AnnotationTarget`), in kotlinc's `KotlinTarget`
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KotlinTarget {
    Class,
    AnnotationClass,
    TypeParameter,
    Property,
    Field,
    LocalVariable,
    ValueParameter,
    Constructor,
    Function,
    PropertyGetter,
    PropertySetter,
    Type,
    Expression,
    File,
    Typealias,
}

impl KotlinTarget {
    /// Every target, in order.
    const ALL: [Self; 15] = [
        Self::Class,
        Self::AnnotationClass,
        Self::TypeParameter,
        Self::Property,
        Self::Field,
        Self::LocalVariable,
        Self::ValueParameter,
        Self::Constructor,
        Self::Function,
        Self::PropertyGetter,
        Self::PropertySetter,
        Self::Type,
        Self::Expression,
        Self::File,
        Self::Typealias,
    ];

    /// The target an `AnnotationTarget` entry names.
    pub fn from_entry(entry: &str) -> Option<Self> {
        Some(match entry {
            "CLASS" => Self::Class,
            "ANNOTATION_CLASS" => Self::AnnotationClass,
            "TYPE_PARAMETER" => Self::TypeParameter,
            "PROPERTY" => Self::Property,
            "FIELD" => Self::Field,
            "LOCAL_VARIABLE" => Self::LocalVariable,
            "VALUE_PARAMETER" => Self::ValueParameter,
            "CONSTRUCTOR" => Self::Constructor,
            "FUNCTION" => Self::Function,
            "PROPERTY_GETTER" => Self::PropertyGetter,
            "PROPERTY_SETTER" => Self::PropertySetter,
            "TYPE" => Self::Type,
            "EXPRESSION" => Self::Expression,
            "FILE" => Self::File,
            "TYPEALIAS" => Self::Typealias,
            _ => return None,
        })
    }

    /// The targets a Java `ElementType` entry stands for (kotlinc's `JavaAnnotationTargetMapper`).
    fn of_java_element_type(entry: &str) -> &'static [Self] {
        match entry {
            "TYPE" => &[Self::Class, Self::File],
            "ANNOTATION_TYPE" => &[Self::AnnotationClass],
            "TYPE_PARAMETER" => &[Self::TypeParameter],
            "FIELD" => &[Self::Field],
            "LOCAL_VARIABLE" => &[Self::LocalVariable],
            "PARAMETER" => &[Self::ValueParameter],
            "CONSTRUCTOR" => &[Self::Constructor],
            "METHOD" => &[Self::Function, Self::PropertyGetter, Self::PropertySetter],
            "TYPE_USE" => &[Self::Type],
            _ => &[],
        }
    }

    /// The target's name in kotlinc's diagnostics.
    pub fn description(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::AnnotationClass => "annotation class",
            Self::TypeParameter => "type parameter",
            Self::Property => "property",
            Self::Field => "field",
            Self::LocalVariable => "local variable",
            Self::ValueParameter => "value parameter",
            Self::Constructor => "constructor",
            Self::Function => "function",
            Self::PropertyGetter => "getter",
            Self::PropertySetter => "setter",
            Self::Type => "type usage",
            Self::Expression => "expression",
            Self::File => "file",
            Self::Typealias => "typealias",
        }
    }
}

/// An annotation class's applicable targets, in the order kotlinc lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnotationTargets {
    targets: [KotlinTarget; 15],
    len: u8,
    /// Whether an application written on a property may land on the PROPERTY itself. Java has no
    /// properties, so kotlinc places a bare Java annotation on the parameter or backing field
    /// instead, even where its default targets list `property`.
    property_site: bool,
}

/// Where an annotation written on a property declaration with no use-site prefix belongs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyAnnotationSite {
    /// A primary-constructor `val`/`var` parameter's own annotation — `RuntimeVisible…
    /// ParameterAnnotations` on the constructor.
    ValueParameter,
    /// The Kotlin PROPERTY, which has no class-file declaration of its own: the annotation goes on a
    /// synthetic `get<Name>$annotations()` marker method.
    Property,
    /// The backing field.
    Field,
}

impl AnnotationTargets {
    /// A Kotlin annotation class that declares no `@Target` (kotlinc's `DEFAULT_TARGET_SET`).
    pub const DEFAULT: Self = Self {
        targets: [
            KotlinTarget::Class,
            KotlinTarget::AnnotationClass,
            KotlinTarget::Property,
            KotlinTarget::Field,
            KotlinTarget::LocalVariable,
            KotlinTarget::ValueParameter,
            KotlinTarget::Constructor,
            KotlinTarget::Function,
            KotlinTarget::PropertyGetter,
            KotlinTarget::PropertySetter,
            KotlinTarget::Class,
            KotlinTarget::Class,
            KotlinTarget::Class,
            KotlinTarget::Class,
            KotlinTarget::Class,
        ],
        len: 10,
        property_site: true,
    };

    /// A Kotlin `@Target(…)`: its entries in declared order, each listed once.
    pub fn kotlin(declared: impl IntoIterator<Item = KotlinTarget>) -> Self {
        let mut targets = Self {
            len: 0,
            ..Self::DEFAULT
        };
        for target in declared {
            targets.push(target);
        }
        targets
    }

    /// A Java `@interface`'s `@java.lang.annotation.Target` entries (`ElementType` names). The
    /// Kotlin targets they map to are listed in target order, then `expression`, which every Java
    /// annotation allows; one without a `@Target` allows the default set.
    pub fn java(element_types: &[String]) -> Self {
        let mut targets = if element_types.is_empty() {
            Self::DEFAULT
        } else {
            let mut targets = Self::kotlin([]);
            for target in KotlinTarget::ALL {
                if element_types
                    .iter()
                    .any(|entry| KotlinTarget::of_java_element_type(entry).contains(&target))
                {
                    targets.push(target);
                }
            }
            targets
        };
        targets.push(KotlinTarget::Expression);
        targets.property_site = false;
        targets
    }

    fn push(&mut self, target: KotlinTarget) {
        if !self.allows(target) {
            self.targets[usize::from(self.len)] = target;
            self.len += 1;
        }
    }

    /// The applicable targets, in kotlinc's order.
    pub fn applicable(&self) -> &[KotlinTarget] {
        &self.targets[..usize::from(self.len)]
    }

    pub fn allows(self, target: KotlinTarget) -> bool {
        self.applicable().contains(&target)
    }

    /// Kotlin's use-site default for an annotation written on a property declaration: the first
    /// applicable of `param` (a primary-constructor property parameter only) → `property` → `field`.
    /// `None` when the annotation targets none of the three (its application is a frontend error).
    pub fn property_declaration_site(
        self,
        on_constructor_parameter: bool,
    ) -> Option<PropertyAnnotationSite> {
        if on_constructor_parameter && self.allows(KotlinTarget::ValueParameter) {
            return Some(PropertyAnnotationSite::ValueParameter);
        }
        if self.property_site && self.allows(KotlinTarget::Property) {
            return Some(PropertyAnnotationSite::Property);
        }
        self.allows(KotlinTarget::Field)
            .then_some(PropertyAnnotationSite::Field)
    }
}

#[cfg(test)]
mod tests {
    use super::{AnnotationTargets, KotlinTarget};

    fn described(targets: AnnotationTargets) -> Vec<&'static str> {
        targets
            .applicable()
            .iter()
            .map(|target| target.description())
            .collect()
    }

    /// kotlinc lists a Kotlin `@Target` in declared order, once per target.
    #[test]
    fn a_kotlin_target_keeps_its_declared_order() {
        let targets = AnnotationTargets::kotlin([
            KotlinTarget::Function,
            KotlinTarget::Field,
            KotlinTarget::Function,
            KotlinTarget::Class,
        ]);
        assert_eq!(described(targets), ["function", "field", "class"]);
    }

    /// A Java `@Target` lists its mapped targets in target order, then `expression`.
    #[test]
    fn a_java_target_maps_its_element_types_like_kotlinc() {
        let targets = AnnotationTargets::java(&["FIELD".into(), "TYPE".into()]);
        assert_eq!(described(targets), ["class", "field", "file", "expression"]);
        let targets = AnnotationTargets::java(&["METHOD".into(), "FIELD".into()]);
        assert_eq!(
            described(targets),
            ["field", "function", "getter", "setter", "expression"]
        );
        assert_eq!(
            targets.property_declaration_site(false),
            Some(super::PropertyAnnotationSite::Field)
        );
    }
}
