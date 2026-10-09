//! Maven artifacts as the toolchain reads them: coordinates and versions, the shared artifact
//! cache, POMs and Gradle module metadata, and the dependencies and constraints one artifact
//! declares for a resolution scope.

mod artifact;
mod comparable_version;
mod coordinates;
mod effective_pom;
mod gradle_module;
mod metadata;
mod pom;
mod store;
mod system;
mod variants;
mod version;

pub use artifact::{
    Artifact, ArtifactId, ArtifactResolver, Constraint, ConstraintId, Declared, Scope,
};
pub use comparable_version::ComparableVersion;
pub use coordinates::Coordinates;
pub use metadata::Metadata;
pub use store::Store;
pub use version::{single_version, RichVersion};
