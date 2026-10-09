//! A Maven artifact's coordinates, printed as the toolchain prints them.

/// Packaging types whose artifact is a `.jar` (Maven's artifact handlers, and `bundle`).
const JAR_PACKAGINGS: [&str; 10] = [
    "test-jar",
    "ejb",
    "ejb-client",
    "maven-plugin",
    "bundle",
    "classpath-jar",
    "module-jar",
    "processor",
    "classpath-processor",
    "modular-processor",
];

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Coordinates {
    pub group: String,
    pub artifact: String,
    /// `None` when unspecified (then taken from a BOM).
    pub version: Option<String>,
    pub classifier: Option<String>,
    /// The declared packaging type, used only when resolving through a POM.
    pub packaging: Option<String>,
}

impl Coordinates {
    /// Coordinates with their parts trimmed (`mavenCoordinatesTrimmed`).
    pub fn new(group: &str, artifact: &str, version: Option<&str>) -> Self {
        Self {
            group: group.trim().to_string(),
            artifact: artifact.trim().to_string(),
            version: version.map(|version| version.trim().to_string()),
            classifier: None,
            packaging: None,
        }
    }

    /// Coordinates with a classifier and packaging type, all parts trimmed.
    pub fn with_selector(
        group: &str,
        artifact: &str,
        version: Option<&str>,
        classifier: Option<&str>,
        packaging: Option<&str>,
    ) -> Self {
        Self {
            classifier: classifier.map(|classifier| classifier.trim().to_string()),
            packaging: packaging.map(|packaging| packaging.trim().to_string()),
            ..Self::new(group, artifact, version)
        }
    }

    /// The coordinates the artifact's POM and module metadata are published under: without its
    /// classifier.
    pub fn metadata(&self) -> Self {
        Self {
            classifier: None,
            ..self.clone()
        }
    }

    pub fn with_version(&self, version: Option<&str>) -> Self {
        Self {
            version: version.map(str::to_string),
            ..self.clone()
        }
    }

    /// `group:artifact`, the key conflicts are resolved by.
    pub fn key(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }

    /// `group:artifact:version[ -> resolved][:classifier][@packaging]` (`toPrettyString`); the
    /// packaging is printed only when its artifact is not a jar.
    pub fn pretty(&self, resolved: Option<&str>) -> String {
        let mut text = format!(
            "{}:{}:{}",
            self.group,
            self.artifact,
            self.version.as_deref().unwrap_or("unspecified")
        );
        if let Some(resolved) = resolved {
            text.push_str(&format!(" -> {resolved}"));
        }
        if let Some(classifier) = &self.classifier {
            text.push_str(&format!(":{classifier}"));
        }
        if let Some(packaging) = &self.packaging {
            if artifact_extension(packaging) != "jar" {
                text.push_str(&format!("@{packaging}"));
            }
        }
        text
    }

    /// The file name of this artifact without its extension: `artifact-version[-classifier]`.
    pub fn file_stem(&self) -> String {
        let mut stem = format!(
            "{}-{}",
            self.artifact,
            self.version.as_deref().unwrap_or("unspecified")
        );
        if let Some(classifier) = &self.classifier {
            stem.push_str(&format!("-{classifier}"));
        }
        stem
    }
}

/// The extension of the artifact a packaging type produces (`resolveArtifactExtension`).
pub fn artifact_extension(packaging: &str) -> &str {
    if packaging == "jar" || JAR_PACKAGINGS.contains(&packaging) || packaging.ends_with("-plugin") {
        "jar"
    } else {
        packaging
    }
}

#[cfg(test)]
mod tests {
    use super::Coordinates;

    #[test]
    fn coordinates_print_as_the_toolchain_prints_them() {
        let mut coordinates = Coordinates::new("org.example", "lib", Some("1.0"));
        assert_eq!(coordinates.pretty(None), "org.example:lib:1.0");
        assert_eq!(
            coordinates.pretty(Some("2.0")),
            "org.example:lib:1.0 -> 2.0"
        );
        coordinates.classifier = Some("linux".to_string());
        coordinates.packaging = Some("bundle".to_string());
        assert_eq!(coordinates.pretty(None), "org.example:lib:1.0:linux");
        coordinates.packaging = Some("aar".to_string());
        assert_eq!(coordinates.pretty(None), "org.example:lib:1.0:linux@aar");
        assert_eq!(
            Coordinates::new("g", "a", None).pretty(None),
            "g:a:unspecified"
        );
    }
}
