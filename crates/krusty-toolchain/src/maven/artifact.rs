//! What one artifact declares for a resolution scope (`MavenDependencyImpl.resolve`): the
//! artifacts it depends on and the version constraints it imposes, read from its Gradle module
//! metadata when it publishes one, else from its effective POM.

use std::collections::HashMap;
use std::hash::Hash;
use std::path::Path;
use std::rc::Rc;

use super::gradle_module::{self, Variant};
use super::metadata::missing;
use super::pom::Pom;
use super::variants::{without_documentation_and_metadata, Requested};
use super::{Coordinates, Metadata, Problem, RichVersion, StoredFile};

/// The POM marker of a library whose Gradle module metadata is authoritative.
const GRADLE_METADATA_MARKER: &str = "do_not_remove: published-with-gradle-metadata";

/// A classpath a graph is resolved for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Compile,
    Runtime,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Scope::Compile => "COMPILE",
            Scope::Runtime => "RUNTIME",
        }
    }

    /// The scope whose variants serve when this one has none.
    pub fn other(self) -> Scope {
        match self {
            Scope::Compile => Scope::Runtime,
            Scope::Runtime => Scope::Compile,
        }
    }

    /// Whether a POM dependency with `scope` is on this classpath.
    fn admits(self, scope: Option<&str>) -> bool {
        match self {
            Scope::Compile => matches!(scope, None | Some("compile")),
            Scope::Runtime => matches!(scope, None | Some("compile" | "runtime")),
        }
    }
}

/// A requested artifact: a library, or a platform (BOM) whose constraints apply to the graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Artifact {
    pub coordinates: Coordinates,
    pub is_bom: bool,
}

/// A constraint on the version of `group:module` wherever it is in the graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Constraint {
    pub group: String,
    pub module: String,
    pub version: RichVersion,
}

impl Constraint {
    pub fn key(&self) -> String {
        format!("{}:{}", self.group, self.module)
    }
}

/// An artifact a resolver has seen, interned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ArtifactId(u32);

/// A constraint a resolver has seen, interned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConstraintId(u32);

/// What an artifact declares for a scope.
#[derive(Debug, Default)]
pub struct Declared {
    pub children: Vec<ArtifactId>,
    pub constraints: Vec<ConstraintId>,
    /// Why the artifact could not be (completely) read.
    pub problems: Vec<Problem>,
    /// The artifacts reading it read too.
    reads: Vec<(ArtifactId, Scope)>,
}

/// What reading an artifact's metadata finds, before it is interned.
#[derive(Default)]
struct Read {
    children: Vec<Artifact>,
    constraints: Vec<Constraint>,
    problems: Vec<Problem>,
}

/// Values numbered in the order they are first seen.
struct Interner<T> {
    ids: HashMap<T, u32>,
    values: Vec<T>,
}

impl<T> Default for Interner<T> {
    fn default() -> Self {
        Self {
            ids: HashMap::new(),
            values: Vec::new(),
        }
    }
}

impl<T: Clone + Eq + Hash> Interner<T> {
    fn intern(&mut self, value: &T) -> u32 {
        if let Some(&id) = self.ids.get(value) {
            return id;
        }
        let id = self.values.len() as u32;
        self.values.push(value.clone());
        self.ids.insert(value.clone(), id);
        id
    }
}

fn scope_index(scope: Scope) -> usize {
    match scope {
        Scope::Compile => 0,
        Scope::Runtime => 1,
    }
}

/// Reads what artifacts declare, for the modules with one JDK version, which decides POM
/// profiles, and one `excludeDependencies` list, whose artifacts are never followed. What an
/// artifact declares is read once; each resolution sees it as read only once it reads it.
pub struct ArtifactResolver<'a> {
    metadata: &'a Metadata<'a>,
    jdk_version: String,
    blocklist: Vec<(String, String)>,
    artifacts: Interner<Artifact>,
    constraints: Interner<Constraint>,
    /// By artifact, for the compile and the runtime scope.
    declared: Vec<[Option<Rc<Declared>>; 2]>,
    /// Whether the current resolution read the artifact, by artifact and scope.
    read: Vec<[bool; 2]>,
    /// For each artifact being read, the artifacts its reading read.
    recording: Vec<Vec<(ArtifactId, Scope)>>,
}

impl<'a> ArtifactResolver<'a> {
    pub fn new(
        metadata: &'a Metadata<'a>,
        jdk_version: String,
        blocklist: Vec<(String, String)>,
    ) -> Self {
        Self {
            metadata,
            jdk_version,
            blocklist,
            artifacts: Interner::default(),
            constraints: Interner::default(),
            declared: Vec::new(),
            read: Vec::new(),
            recording: Vec::new(),
        }
    }

    /// Whether the resolver reads for modules with `jdk_version` and `blocklist`.
    pub fn serves(&self, jdk_version: &str, blocklist: &[(String, String)]) -> bool {
        self.jdk_version == jdk_version && self.blocklist == blocklist
    }

    /// Start a resolution: nothing is read in it yet.
    pub fn forget_reads(&mut self) {
        self.read.fill([false; 2]);
    }

    pub fn intern(&mut self, artifact: &Artifact) -> ArtifactId {
        let id = self.artifacts.intern(artifact);
        if self.declared.len() <= id as usize {
            self.declared.resize(id as usize + 1, [None, None]);
            self.read.resize(id as usize + 1, [false; 2]);
        }
        ArtifactId(id)
    }

    pub fn artifact(&self, id: ArtifactId) -> &Artifact {
        &self.artifacts.values[id.0 as usize]
    }

    pub fn constraint(&self, id: ConstraintId) -> &Constraint {
        &self.constraints.values[id.0 as usize]
    }

    fn mark_read(&mut self, artifact: ArtifactId, scope: Scope) {
        self.read[artifact.0 as usize][scope_index(scope)] = true;
        if let Some(reads) = self.recording.last_mut() {
            reads.push((artifact, scope));
        }
    }

    fn blocked(&self, group: &str, module: &str) -> bool {
        self.blocklist
            .iter()
            .any(|(blocked_group, blocked_module)| {
                blocked_group == group && blocked_module == module
            })
    }

    /// Whether the current resolution has read what `artifact` declares for `scope`.
    pub fn is_read(&self, artifact: ArtifactId, scope: Scope) -> bool {
        self.read[artifact.0 as usize][scope_index(scope)]
    }

    /// What `artifact` declares for `scope`, if the current resolution has read it.
    pub fn read_declared(&self, artifact: ArtifactId, scope: Scope) -> Option<Rc<Declared>> {
        if !self.is_read(artifact, scope) {
            return None;
        }
        self.declared[artifact.0 as usize][scope_index(scope)].clone()
    }

    /// What `artifact` declares for `scope`; an artifact without a version declares nothing.
    pub fn declared(&mut self, artifact: ArtifactId, scope: Scope) -> Rc<Declared> {
        if let Some(declared) = self.declared[artifact.0 as usize][scope_index(scope)].clone() {
            self.mark_read(artifact, scope);
            for &(read, scope) in &declared.reads {
                self.mark_read(read, scope);
            }
            return declared;
        }
        // A placeholder breaks cycles through BOMs that import each other.
        self.declared[artifact.0 as usize][scope_index(scope)] = Some(Rc::new(Declared::default()));
        self.mark_read(artifact, scope);
        self.recording.push(Vec::new());
        let read = self.read(&self.artifact(artifact).clone(), scope);
        let reads = self.recording.pop().unwrap_or_default();
        let declared = Rc::new(Declared {
            children: read
                .children
                .iter()
                .map(|child| self.intern(child))
                .collect(),
            constraints: read
                .constraints
                .iter()
                .map(|constraint| ConstraintId(self.constraints.intern(constraint)))
                .collect(),
            problems: read.problems,
            reads,
        });
        self.declared[artifact.0 as usize][scope_index(scope)] = Some(declared.clone());
        declared
    }

    fn read(&mut self, artifact: &Artifact, scope: Scope) -> Read {
        let coordinates = &artifact.coordinates;
        if coordinates.version.is_none() {
            return Read::default();
        }
        let metadata = coordinates.metadata();
        let failed = |problem: Problem| Read {
            problems: vec![problem],
            ..Read::default()
        };
        let pom = match self.metadata.file(&metadata, "pom") {
            Ok(pom) => pom,
            Err(problem) => return failed(problem),
        };
        let gradle_metadata = pom
            .as_ref()
            .is_none_or(|pom| pom.text.contains(GRADLE_METADATA_MARKER));
        if gradle_metadata {
            match self.metadata.file(&metadata, "module") {
                Ok(Some(module)) => {
                    return self.read_module(artifact, scope, &module, pom.is_some())
                }
                Ok(None) => {}
                Err(problem) => return failed(problem),
            }
        }
        match pom {
            Some(_) => self.read_pom(artifact, scope),
            None => failed(missing(coordinates, "POM or module metadata")),
        }
    }

    /// `resolveUsingPom`: a BOM's managed versions become constraints; a library's dependencies in
    /// `scope` its children.
    fn read_pom(&mut self, artifact: &Artifact, scope: Scope) -> Read {
        let effective = match self
            .metadata
            .effective_pom(&artifact.coordinates.metadata(), &self.jdk_version)
        {
            Ok(effective) => effective,
            Err(problems) => {
                return Read {
                    problems,
                    ..Read::default()
                }
            }
        };
        let project = &effective.pom;
        let mut declared = Read {
            problems: effective.problems.clone(),
            ..Read::default()
        };
        if artifact.is_bom {
            declared.constraints = pom_constraints(project);
        } else {
            declared.children = project
                .dependencies
                .iter()
                .filter(|dependency| scope.admits(dependency.scope.as_deref()))
                .filter(|dependency| !self.blocked(&dependency.group, &dependency.artifact))
                .filter(|dependency| dependency.optional != Some(true))
                .map(|dependency| Artifact {
                    coordinates: Coordinates::with_selector(
                        &dependency.group,
                        &dependency.artifact,
                        dependency.version.as_deref(),
                        dependency.classifier.as_deref(),
                        dependency.packaging.as_deref(),
                    ),
                    is_bom: false,
                })
                .collect();
        }
        declared
    }

    /// `resolveUsingMetadata` for the JVM.
    fn read_module(
        &mut self,
        artifact: &Artifact,
        scope: Scope,
        file: &StoredFile,
        has_pom: bool,
    ) -> Read {
        let variants: Rc<[Variant]> = match self
            .metadata
            .variants(&artifact.coordinates.metadata(), file)
        {
            Ok(variants) => variants,
            Err(problem) => {
                return Read {
                    problems: vec![problem],
                    ..Read::default()
                }
            }
        };
        let in_file = |message: String| Problem::in_file(&file.path, None, message);
        let coordinates = &artifact.coordinates;
        let version = coordinates.version.clone().unwrap_or_default();
        let requested = Requested {
            group: &coordinates.group,
            module: &coordinates.artifact,
            version: &version,
            is_bom: artifact.is_bom,
        };
        let context = VariantContext {
            artifact,
            scope,
            has_pom,
            source: &file.path,
        };
        let mut declared = Read::default();
        let special = coordinates.group == "org.jetbrains.kotlin"
            && matches!(
                coordinates.artifact.as_str(),
                "kotlin-stdlib-common" | "kotlin-test-annotations-common"
            );
        if special {
            // Libraries for every platform that publish no platform attributes.
            for variant in variants.iter() {
                declared.children.extend(self.variant_dependencies(
                    variant,
                    &context,
                    &mut declared.problems,
                ));
                declared.constraints.extend(constraints(variant));
            }
        } else if artifact.is_bom {
            let valid = requested.bom_variants(&variants, scope);
            if valid.is_empty() {
                declared.problems.push(in_file(format!(
                    "{} is declared as a BOM, but it is a regular library",
                    coordinates.pretty(None)
                )));
            }
            for variant in valid
                .iter()
                .filter(|variant| !variant.is_documentation_or_metadata())
            {
                declared.constraints.extend(constraints(variant));
            }
            for variant in &valid {
                declared.children.extend(self.variant_dependencies(
                    variant,
                    &context,
                    &mut declared.problems,
                ));
            }
        } else {
            let valid = requested.variants(&variants, scope);
            if valid.is_empty() {
                declared.problems.push(in_file(format!(
                    "No variant of {} matches the JVM {} classpath",
                    coordinates.pretty(None),
                    scope.name().to_lowercase()
                )));
            } else if without_documentation_and_metadata(&valid) > 1 {
                declared.problems.push(in_file(format!(
                    "More than one variant of {} matches the JVM {} classpath: {}",
                    coordinates.pretty(None),
                    scope.name().to_lowercase(),
                    valid
                        .iter()
                        .filter(|variant| !variant.is_documentation_or_metadata())
                        .map(|variant| variant.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            for variant in valid
                .iter()
                .filter(|variant| !variant.is_documentation_or_metadata())
            {
                declared.children.extend(self.variant_dependencies(
                    variant,
                    &context,
                    &mut declared.problems,
                ));
                if let Some(available) = &variant.available_at {
                    if !self.blocked(&available.group, &available.module) {
                        let coordinates = Coordinates::new(
                            &available.group,
                            &available.module,
                            Some(&available.version),
                        );
                        match coordinates.repository_path_problem("jar") {
                            Some(message) => declared.problems.push(in_file(message)),
                            None => declared.children.push(Artifact {
                                coordinates,
                                is_bom: false,
                            }),
                        }
                    }
                }
                declared.constraints.extend(constraints(variant));
            }
        }
        declared
    }

    /// A variant's dependencies, each with a version (`Variant.dependencies`): its own, else its
    /// variant's constraint, else a constraint of the BOMs it depends on.
    fn variant_dependencies(
        &mut self,
        variant: &Variant,
        context: &VariantContext<'_>,
        problems: &mut Vec<Problem>,
    ) -> Vec<Artifact> {
        let mut bom_constraints: Option<Vec<Constraint>> = None;
        let mut artifacts = Vec::new();
        for dependency in &variant.dependencies {
            if self.blocked(&dependency.group, &dependency.module) {
                continue;
            }
            let version = match &dependency.version {
                Some(version) => Some(version.clone()),
                None => variant_constraint(variant, dependency).or_else(|| {
                    let constraints = bom_constraints
                        .get_or_insert_with(|| self.bom_constraints(variant, context, problems));
                    constraints
                        .iter()
                        .find(|constraint| {
                            constraint.group == dependency.group
                                && constraint.module == dependency.module
                        })
                        .map(|constraint| constraint.version.clone())
                }),
            };
            artifacts.extend(self.dependency_artifact(
                dependency,
                version.as_ref(),
                context,
                problems,
            ));
        }
        artifacts
    }

    /// The constraints of the BOMs a variant depends on, else its POM's managed versions
    /// (`bomDependencyConstraints`).
    fn bom_constraints(
        &mut self,
        variant: &Variant,
        context: &VariantContext<'_>,
        problems: &mut Vec<Problem>,
    ) -> Vec<Constraint> {
        let mut found = Vec::new();
        for dependency in variant
            .dependencies
            .iter()
            .filter(|dependency| dependency.is_bom())
        {
            let version = dependency
                .version
                .clone()
                .or_else(|| variant_constraint(variant, dependency));
            let Some(version) = version else { continue };
            let bom = Artifact {
                coordinates: Coordinates::with_selector(
                    &dependency.group,
                    &dependency.module,
                    version.resolve().as_deref(),
                    dependency.classifier.as_deref(),
                    dependency.extension.as_deref(),
                ),
                is_bom: true,
            };
            let extension = dependency.extension.as_deref().unwrap_or("jar");
            if let Some(message) = bom.coordinates.repository_path_problem(extension) {
                problems.push(Problem::in_file(context.source, None, message));
                continue;
            }
            let bom = self.intern(&bom);
            let declared = self.declared(bom, context.scope);
            found.extend(
                declared
                    .constraints
                    .iter()
                    .map(|&id| self.constraint(id).clone()),
            );
        }
        if found.is_empty() && context.has_pom {
            match self
                .metadata
                .effective_pom(&context.artifact.coordinates.metadata(), &self.jdk_version)
            {
                Ok(effective) => {
                    problems.extend(effective.problems.iter().cloned());
                    return pom_constraints(&effective.pom);
                }
                Err(failures) => problems.extend(failures),
            }
        }
        found
    }

    /// The artifact a Gradle dependency names (`toMavenDependency`).
    fn dependency_artifact(
        &self,
        dependency: &gradle_module::Dependency,
        version: Option<&RichVersion>,
        context: &VariantContext<'_>,
        problems: &mut Vec<Problem>,
    ) -> Option<Artifact> {
        let resolved = version.and_then(RichVersion::resolve);
        if version.is_some() && resolved.is_none() {
            problems.push(Problem::in_file(
                context.source,
                None,
                format!(
                    "Unable to determine the version of {}:{} required by {}: no version attribute is defined",
                    dependency.group,
                    dependency.module,
                    context.artifact.coordinates.pretty(None)
                ),
            ));
        }
        let coordinates = Coordinates::with_selector(
            &dependency.group,
            &dependency.module,
            resolved.as_deref(),
            dependency.classifier.as_deref(),
            dependency.extension.as_deref(),
        );
        let extension = dependency.extension.as_deref().unwrap_or("jar");
        if let Some(message) = coordinates.repository_path_problem(extension) {
            problems.push(Problem::in_file(context.source, None, message));
            return None;
        }
        Some(Artifact {
            coordinates,
            is_bom: dependency.is_bom(),
        })
    }
}

/// The artifact whose variants are read.
struct VariantContext<'c> {
    artifact: &'c Artifact,
    scope: Scope,
    /// Whether the artifact also publishes a POM, whose managed versions complete its variants'.
    has_pom: bool,
    source: &'c Path,
}

/// The version the variant's own constraints give `dependency`.
fn variant_constraint(
    variant: &Variant,
    dependency: &gradle_module::Dependency,
) -> Option<RichVersion> {
    variant
        .dependency_constraints
        .iter()
        .find(|constraint| {
            constraint.module == dependency.module
                && constraint.group == dependency.group
                && constraint.version.is_some()
        })
        .and_then(|constraint| constraint.version.clone())
}

/// A variant's constraints that have a version.
fn constraints(variant: &Variant) -> impl Iterator<Item = Constraint> + '_ {
    variant
        .dependency_constraints
        .iter()
        .filter_map(|constraint| {
            Some(Constraint {
                group: constraint.group.clone(),
                module: constraint.module.clone(),
                version: constraint.version.clone()?,
            })
        })
}

/// A BOM POM's constraints: its managed versions (`resolveDependenciesConstraints`).
fn pom_constraints(project: &Pom) -> Vec<Constraint> {
    project
        .dependency_management
        .iter()
        .filter(|managed| managed.optional != Some(true))
        .filter_map(|managed| {
            Some(Constraint {
                group: managed.group.clone(),
                module: managed.artifact.clone(),
                version: RichVersion::requires(managed.version.as_deref()?),
            })
        })
        .collect()
}
