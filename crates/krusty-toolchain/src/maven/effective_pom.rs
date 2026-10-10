//! The effective POM (`pomResolver.kt`): a POM with its parents, active profiles, imported
//! dependency management and property references applied, and each dependency's version and scope
//! completed from dependency management.

use std::path::Path;

use super::coordinates::artifact_extension;
use super::metadata::Metadata;
use super::pom::{merge_dependencies, Activation, Dependency, Pom, Profile, Properties};
use super::system::{self, OsParameter};
use super::{single_version, Coordinates, Problem};

/// More ancestors than this are not read (`ProjectHasMoreThanTenAncestors`).
const MAX_DEPTH: usize = 10;
/// Bounds property references that expand to themselves.
const MAX_EXPANSIONS: usize = 64;

/// An effective POM, and the problems reading its ancestors and imports.
pub struct EffectivePom {
    pub pom: Pom,
    pub problems: Vec<Problem>,
    /// How many levels of ancestors and imports were read below it.
    pub height: usize,
}

/// What the effective POM of one artifact reads besides its own POM.
pub struct Reading<'a> {
    metadata: &'a Metadata<'a>,
    /// The module's JDK version, as profiles' `<jdk>` compare it (`1.8`, `11`, `25`).
    jdk_version: &'a str,
    /// Problems with the POMs read, reported against the artifact.
    pub problems: Vec<Problem>,
    /// The deepest level read so far.
    pub deepest: usize,
}

impl<'a> Reading<'a> {
    pub fn new(metadata: &'a Metadata<'a>, jdk_version: &'a str) -> Self {
        Self {
            metadata,
            jdk_version,
            problems: Vec::new(),
            deepest: 0,
        }
    }

    /// The effective POM of the ancestor or import `coordinates` of a POM `depth` levels down.
    /// The one read for every POM that reads it is used when its own ancestors and imports stay
    /// within the depth limit from here, as reading it again would.
    fn effective_of(&mut self, coordinates: &Coordinates, depth: usize) -> Option<Pom> {
        let raw = match self.metadata.pom(coordinates) {
            Ok(raw) => raw,
            Err(problem) => {
                self.problems.push(problem);
                return None;
            }
        };
        if let Some(Ok(shared)) = self
            .metadata
            .shared_effective_pom(coordinates, self.jdk_version)
        {
            if depth + shared.height <= MAX_DEPTH {
                self.problems.extend(shared.problems.iter().cloned());
                self.deepest = self.deepest.max(depth + shared.height);
                return Some(shared.pom.clone());
            }
        }
        Some(self.effective(&raw.pom, &raw.path, depth))
    }

    /// The effective POM of `pom` (`Project.resolve`).
    pub fn effective(&mut self, pom: &Pom, source: &Path, depth: usize) -> Pom {
        self.deepest = self.deepest.max(depth);
        if depth > MAX_DEPTH {
            self.problems.push(Problem::in_file(
                source,
                None,
                format!(
                    "The POM of {}:{}:{} has more than ten ancestors; the rest are not read",
                    pom.group.as_deref().unwrap_or_default(),
                    pom.artifact.as_deref().unwrap_or_default(),
                    pom.version.as_deref().unwrap_or_default()
                ),
            ));
            return pom.clone();
        }
        let parent = pom.parent.as_ref().and_then(|parent| {
            let coordinates =
                Coordinates::new(&parent.group, &parent.artifact, Some(&parent.version));
            if let Some(message) = coordinates.repository_path_problem("pom") {
                self.problems
                    .push(Problem::in_file(source, Some(parent.position), message));
                return None;
            }
            self.effective_of(&coordinates, depth + 1)
        });
        let without_profiles = match &parent {
            Some(parent) => merge_properties(parent.properties.as_ref(), pom.properties.as_ref()),
            None => pom.properties.clone(),
        };
        let active = self.active_profiles(pom, without_profiles.as_ref());
        let profile_properties = active
            .iter()
            .fold(None, |merged: Option<Properties>, profile| {
                merge_properties(merged.as_ref(), profile.properties.as_ref())
            });
        let properties = merge_properties(without_profiles.as_ref(), profile_properties.as_ref());
        let profile_dependencies: Vec<Dependency> = merge_dependencies(
            &active
                .iter()
                .map(|profile| profile.dependencies.as_slice())
                .collect::<Vec<_>>(),
        );
        let profile_management: Vec<Dependency> = merge_dependencies(
            &active
                .iter()
                .map(|profile| profile.dependency_management.as_slice())
                .collect::<Vec<_>>(),
        );
        let mut project = pom.clone();
        project.properties = properties;
        match &parent {
            Some(parent) => {
                project.group = pom.group.clone().or_else(|| parent.group.clone());
                project.artifact = pom.artifact.clone().or_else(|| parent.artifact.clone());
                project.version = pom.version.clone().or_else(|| parent.version.clone());
                project.dependencies = merge_dependencies(&[
                    &profile_dependencies,
                    &pom.dependencies,
                    &parent.dependencies,
                ]);
                let imported = self.imported_management(&project, depth);
                project.dependency_management = merge_dependencies(&[
                    &profile_management,
                    &pom.dependency_management,
                    &imported,
                    &parent.dependency_management,
                ]);
            }
            None => {
                // Without a readable parent, imports see the POM's own properties only.
                let imported = self.imported_management(pom, depth);
                if let Some(declared) = &pom.parent {
                    if pom.group.is_none() || pom.artifact.is_none() || pom.version.is_none() {
                        project.group = pom.group.clone().or(Some(declared.group.clone()));
                        project.artifact = pom.artifact.clone().or(Some(declared.artifact.clone()));
                        project.version = pom.version.clone().or(Some(declared.version.clone()));
                    }
                }
                project.dependencies =
                    merge_dependencies(&[&profile_dependencies, &pom.dependencies]);
                project.dependency_management = merge_dependencies(&[
                    &profile_management,
                    &pom.dependency_management,
                    &imported,
                ]);
            }
        }
        let management: Vec<Dependency> = project
            .dependency_management
            .iter()
            .map(|dependency| expand_dependency(dependency, &project))
            .collect();
        let dependencies = effective_dependencies(&project, &management);
        project.packaging = project
            .packaging
            .as_deref()
            .map(|value| expand(value, &project));
        project.artifact = project
            .artifact
            .as_deref()
            .map(|value| expand(value, &project));
        project.group = project
            .group
            .as_deref()
            .map(|value| expand(value, &project));
        project.version = project
            .version
            .as_deref()
            .map(|value| expand(value, &project));
        project.dependencies = self.safe_dependencies(dependencies, source);
        project.dependency_management = self.safe_dependencies(management, source);
        project
    }

    /// Keep only dependencies whose coordinates can be used at the repository boundary, reporting
    /// an invalid declaration at the POM that owns it rather than later as a project-level error.
    fn safe_dependencies(
        &mut self,
        dependencies: Vec<Dependency>,
        source: &Path,
    ) -> Vec<Dependency> {
        dependencies
            .into_iter()
            .filter_map(|dependency| match dependency_path_problem(&dependency) {
                Some(message) => {
                    self.problems.push(Problem::in_file(
                        source,
                        Some(dependency.position),
                        message,
                    ));
                    None
                }
                None => Some(dependency),
            })
            .collect()
    }

    /// The dependency management of the BOMs `project` imports (`scope` `import`), the first
    /// import winning (`resolveImportedDependencyManagement`).
    fn imported_management(&mut self, project: &Pom, depth: usize) -> Vec<Dependency> {
        let imports: Vec<Dependency> = project
            .dependency_management
            .iter()
            .map(|dependency| expand_dependency(dependency, project))
            .filter(|dependency| {
                dependency.scope.as_deref() == Some("import") && dependency.version.is_some()
            })
            .collect();
        let mut managements = Vec::new();
        for import in imports {
            let coordinates =
                Coordinates::new(&import.group, &import.artifact, import.version.as_deref());
            // The completed effective POM reports this declaration with its source. Do not attempt
            // a repository read first: that would lose the source and create a second problem.
            if dependency_path_problem(&import).is_some() {
                continue;
            }
            if let Some(effective) = self.effective_of(&coordinates, depth + 1) {
                managements.push(effective.dependency_management);
            }
        }
        merge_dependencies(&managements.iter().map(Vec::as_slice).collect::<Vec<_>>())
    }

    /// The profiles in effect (`activeProfiles`): those whose activation holds, else those active
    /// by default.
    fn active_profiles<'p>(
        &self,
        pom: &'p Pom,
        properties: Option<&Properties>,
    ) -> Vec<&'p Profile> {
        let mut project = pom.clone();
        project.properties = properties.cloned();
        let activated: Vec<&Profile> = pom
            .profiles
            .iter()
            .filter(|profile| {
                profile.activations.as_ref().is_some_and(|activations| {
                    !activations.is_empty()
                        && activations
                            .iter()
                            .all(|activation| self.is_active(activation, &project))
                })
            })
            .collect();
        if !activated.is_empty() {
            return activated;
        }
        pom.profiles
            .iter()
            .filter(|profile| {
                profile.activations.as_ref().is_some_and(|activations| {
                    activations
                        .iter()
                        .any(|activation| activation.active_by_default == Some(true))
                })
            })
            .collect()
    }

    fn is_active(&self, activation: &Activation, project: &Pom) -> bool {
        let Activation {
            jdk,
            os,
            property,
            file,
            ..
        } = activation;
        if jdk.is_none() && os.is_none() && property.is_none() && file.is_none() {
            return false;
        }
        jdk.as_deref()
            .is_none_or(|jdk| jdk_matches(jdk, self.jdk_version))
            && os.as_ref().is_none_or(|os| {
                let parameters = [&os.name, &os.family, &os.arch, &os.version];
                parameters.iter().any(|parameter| parameter.is_some())
                    && OsParameter::Name.matches(os.name.as_deref())
                    && OsParameter::Family.matches(os.family.as_deref())
                    && OsParameter::Arch.matches(os.arch.as_deref())
                    && OsParameter::Version.matches(os.version.as_deref())
            })
            && property
                .as_ref()
                .is_none_or(|(name, value)| property_matches(name.as_deref(), value.as_deref()))
            && file
                .as_ref()
                .is_none_or(|(missing, exists)| file_matches(missing, exists, project))
    }
}

/// Why a dependency declaration cannot name the repository file it will read. Imports read a POM
/// by group, artifact and version; regular dependencies use their selector's artifact extension.
fn dependency_path_problem(dependency: &Dependency) -> Option<String> {
    if dependency.scope.as_deref() == Some("import") && dependency.version.is_some() {
        return Coordinates::new(
            &dependency.group,
            &dependency.artifact,
            dependency.version.as_deref(),
        )
        .repository_path_problem("pom");
    }
    let coordinates = Coordinates::with_selector(
        &dependency.group,
        &dependency.artifact,
        dependency.version.as_deref(),
        dependency.classifier.as_deref(),
        dependency.packaging.as_deref(),
    );
    let extension = dependency
        .packaging
        .as_deref()
        .map(artifact_extension)
        .unwrap_or("jar");
    coordinates.repository_path_problem(extension)
}

/// `first + second` for properties: `None` only when both are.
fn merge_properties(first: Option<&Properties>, second: Option<&Properties>) -> Option<Properties> {
    match (first, second) {
        (None, second) => second.cloned(),
        (Some(first), None) => Some(first.clone()),
        (Some(first), Some(second)) => Some(first.merged(second)),
    }
}

/// The project's dependencies with versions and scopes taken from `management`
/// (`getEffectiveDependencies`).
fn effective_dependencies(project: &Pom, management: &[Dependency]) -> Vec<Dependency> {
    let completed: Vec<Dependency> = project
        .dependencies
        .iter()
        .map(|dependency| expand_dependency(dependency, project))
        .map(|dependency| {
            if let (Some(version), Some(_)) = (&dependency.version, &dependency.scope) {
                return Dependency {
                    version: single_version(version),
                    ..dependency
                };
            }
            let Some(managed) = management.iter().find(|managed| {
                managed.group == dependency.group && managed.artifact == dependency.artifact
            }) else {
                return dependency;
            };
            let mut completed = dependency.clone();
            if dependency.version.is_none() {
                if let Some(version) = managed.version.as_deref().and_then(single_version) {
                    completed.version = Some(version);
                }
            }
            if dependency.scope.is_none() && managed.scope.is_some() {
                completed.scope = managed.scope.clone();
            }
            completed
        })
        .map(|dependency| expand_dependency(&dependency, project))
        .collect();
    merge_dependencies(&[&completed])
}

/// A dependency with its property references expanded (`Dependency.expandTemplates`); a value
/// that expands to blank is absent, and a version range is cut to the version it stands for.
fn expand_dependency(dependency: &Dependency, project: &Pom) -> Dependency {
    let optional = |value: &Option<String>| {
        value
            .as_deref()
            .map(|value| expand(value, project))
            .filter(|value| !value.trim().is_empty())
    };
    Dependency {
        group: expand(&dependency.group, project),
        artifact: expand(&dependency.artifact, project),
        version: optional(&dependency.version).and_then(|version| single_version(&version)),
        optional: dependency.optional,
        packaging: optional(&dependency.packaging),
        classifier: optional(&dependency.classifier),
        scope: optional(&dependency.scope),
        position: dependency.position,
    }
}

/// `value` with its `${…}` references expanded as Maven does (`expandTemplate`): the project's
/// built-in properties, then its declared properties, then system properties and environment
/// variables. A reference that resolves to nothing leaves the value as written.
pub fn expand(value: &str, project: &Pom) -> String {
    expand_bounded(value, project, MAX_EXPANSIONS)
}

fn expand_bounded(value: &str, project: &Pom, budget: usize) -> String {
    let (Some(start), Some(end)) = (value.find("${"), value.find('}')) else {
        return value.to_string();
    };
    if end < start + 2 || budget == 0 {
        return value.to_string();
    }
    let key = &value[start + 2..end];
    let built_in = key
        .strip_prefix("project.")
        .or_else(|| key.strip_prefix("pom."))
        .and_then(|name| built_in_property(name, project));
    let declared = || {
        project
            .properties
            .as_ref()
            .and_then(|properties| properties.get(key))
            .map(|value| value.unwrap_or("").to_string())
    };
    let Some(resolved) = built_in.or_else(declared).or_else(|| system::property(key)) else {
        return value.to_string();
    };
    let expanded = format!(
        "{}{}{}",
        &value[..start],
        expand_bounded(&resolved, project, budget - 1),
        &value[end + 1..]
    );
    expand_bounded(&expanded, project, budget - 1)
}

fn built_in_property(name: &str, project: &Pom) -> Option<String> {
    let parent = project.parent.as_ref();
    match name {
        "groupId" => project
            .group
            .clone()
            .or_else(|| parent.map(|parent| parent.group.clone())),
        "artifactId" => project
            .artifact
            .clone()
            .or_else(|| parent.map(|parent| parent.artifact.clone())),
        "version" => project
            .version
            .clone()
            .or_else(|| parent.map(|parent| parent.version.clone())),
        "prerequisites.maven" => project.prerequisites_maven.clone(),
        "parent.groupId" => parent.map(|parent| parent.group.clone()),
        "parent.artifactId" => parent.map(|parent| parent.artifact.clone()),
        "parent.version" => parent.map(|parent| parent.version.clone()),
        _ => None,
    }
}

/// A `<jdk>` activation (`isActiveJdk`): a prefix, a negated prefix (`!1.8`) or a range.
fn jdk_matches(expected: &str, actual: &str) -> bool {
    if let Some(prefix) = expected.strip_prefix('!') {
        return !actual.starts_with(prefix);
    }
    if expected.starts_with('[') || expected.starts_with('(') {
        return jdk_range(expected).is_some_and(|(low, high)| {
            jdk_version(actual)
                .is_some_and(|version| low.admits(&version, true) && high.admits(&version, false))
        });
    }
    actual.starts_with(expected)
}

/// A JDK version's numeric parts (`toJdkVersion`): everything but digits and `._-` is dropped.
fn jdk_version(version: &str) -> Option<Vec<i64>> {
    let kept: String = version
        .chars()
        .filter(|character| character.is_ascii_digit() || matches!(character, '.' | '_' | '-'))
        .collect();
    kept.split(['.', '_', '-'])
        .map(|part| part.parse().ok())
        .collect()
}

/// One bound of a JDK range; `None` parts are the open ends.
struct Bound {
    version: Option<Vec<i64>>,
    closed: bool,
}

impl Bound {
    fn admits(&self, version: &[i64], lower: bool) -> bool {
        let Some(bound) = &self.version else {
            return true;
        };
        let order = compare_jdk(version, bound);
        match (lower, self.closed) {
            (true, true) => order.is_ge(),
            (true, false) => order.is_gt(),
            (false, true) => order.is_le(),
            (false, false) => order.is_lt(),
        }
    }
}

/// Compare JDK versions part by part, a missing part ordering first.
fn compare_jdk(this: &[i64], other: &[i64]) -> std::cmp::Ordering {
    for index in 0..this.len().max(other.len()) {
        let this_part = this.get(index).copied().unwrap_or(i64::MIN);
        let other_part = other.get(index).copied().unwrap_or(i64::MIN);
        if this_part != other_part {
            return this_part.cmp(&other_part);
        }
    }
    std::cmp::Ordering::Equal
}

fn jdk_range(range: &str) -> Option<(Bound, Bound)> {
    let parts: Vec<&str> = range.split(',').collect();
    if parts.len() > 2 {
        return None;
    }
    let numbers = |value: &str| -> Option<Option<Vec<i64>>> {
        if value.trim().is_empty() {
            return Some(None);
        }
        value
            .split('.')
            .map(|part| part.parse().ok())
            .collect::<Option<Vec<i64>>>()
            .map(Some)
    };
    let left = parts[0].trim();
    let low = Bound {
        version: numbers(left.trim_start_matches(['[', '(']))?,
        closed: left.starts_with('['),
    };
    let high = match parts.get(1) {
        Some(right) => {
            let right = right.trim();
            Bound {
                version: numbers(right.trim_end_matches([']', ')']))?,
                closed: right.ends_with(']'),
            }
        }
        None => Bound {
            version: None,
            closed: false,
        },
    };
    Some((low, high))
}

/// A `<property>` activation: a name that must be absent (`!name`), present, or have a value.
fn property_matches(name: Option<&str>, value: Option<&str>) -> bool {
    let Some(name) = name.filter(|name| !name.trim().is_empty()) else {
        return false;
    };
    if let Some(name) = name.strip_prefix('!') {
        return system::property(name).is_none();
    }
    let actual = system::property(name);
    match value.filter(|value| !value.trim().is_empty()) {
        None => actual.is_some(),
        Some(value) => match value.strip_prefix('!') {
            Some(value) => actual.as_deref() != Some(value),
            None => actual.as_deref() == Some(value),
        },
    }
}

/// A `<file>` activation: an absolute path (a library has no base directory) that must exist or be
/// missing.
fn file_matches(missing: &Option<String>, exists: &Option<String>, project: &Pom) -> bool {
    let present = |value: &Option<String>| value.clone().filter(|value| !value.trim().is_empty());
    let (declared, existing) = match (present(exists), present(missing)) {
        (Some(path), _) => (path, true),
        (None, Some(path)) => (path, false),
        (None, None) => return false,
    };
    if declared.contains("${basedir}") {
        return false;
    }
    let path = expand(&declared, project);
    let path = Path::new(&path);
    path.is_absolute() && path.exists() == existing
}

#[cfg(test)]
mod tests {
    use super::{expand, jdk_matches};
    use crate::maven::pom::parse;

    #[test]
    fn references_expand_from_built_ins_then_properties_and_keep_unknown_ones() {
        let pom = parse(
            "<project><groupId>g</groupId><version>1.2</version>
               <properties><a>${b}-x</a><b>${project.version}</b><none/></properties></project>",
            "g",
            "p",
        )
        .expect("a valid POM");
        assert_eq!(expand("v${a}", &pom), "v1.2-x");
        assert_eq!(expand("[${none}]", &pom), "[]");
        assert_eq!(
            expand("${unknown.property}-${a}", &pom),
            "${unknown.property}-${a}"
        );
        assert_eq!(expand("${pom.groupId}", &pom), "g");
    }

    #[test]
    fn jdk_activations_compare_prefixes_and_ranges() {
        assert!(jdk_matches("1.8", "1.8"));
        assert!(jdk_matches("!1.8", "25"));
        assert!(jdk_matches("[11,)", "25"));
        assert!(!jdk_matches("[1.8,11)", "25"));
        assert!(jdk_matches("(,1.8]", "1.8"));
        assert!(!jdk_matches("[9,", "1.8"));
    }
}
