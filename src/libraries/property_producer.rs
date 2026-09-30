//! How a property read is realized.
//!
//! The classpath provider chooses this while it still knows whether the declaration is storage, a
//! Kotlin accessor, or a Java bean method. Later phases format diagnostics from the choice. They
//! do not recover it from a JVM descriptor.

use super::PropertyInfo;

/// Provider-chosen realization of one property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyProducer {
    /// A physical field. A platform null-check names the field.
    Field,
    /// A declared Kotlin property accessor. A platform null-check names `<get-name>(...)`.
    KotlinAccessor,
    /// A property synthesized from a Java accessor. A platform null-check names `getName(...)`.
    JavaAccessor,
}

impl PropertyProducer {
    /// kotlinc's `checkNotNullExpressionValue` name for a read of this producer.
    pub(crate) fn platform_check_name(self, property: &str, getter_name: Option<&str>) -> String {
        match self {
            Self::Field => getter_name.unwrap_or(property).to_owned(),
            Self::JavaAccessor => format!("{}(...)", getter_name.unwrap_or(property)),
            Self::KotlinAccessor => format!("<get-{property}>(...)"),
        }
    }
}

impl PropertyInfo {
    /// Whether member selection should treat this as a Java bean property rather than a declaration.
    pub(crate) fn accessor_derived(&self) -> bool {
        self.producer == PropertyProducer::JavaAccessor
    }
}

#[cfg(test)]
mod tests {
    use super::PropertyProducer;

    #[test]
    fn platform_check_name_follows_the_producer_not_a_descriptor() {
        assert_eq!(
            PropertyProducer::Field.platform_check_name("value", Some("value")),
            "value"
        );
        assert_eq!(
            PropertyProducer::JavaAccessor.platform_check_name("title", Some("getTitle")),
            "getTitle(...)"
        );
        assert_eq!(
            PropertyProducer::KotlinAccessor.platform_check_name("name", Some("getName")),
            "<get-name>(...)"
        );
    }
}
