//! Metadata read from the store once per run: POM and Gradle module files, parsed POMs and module
//! variants, and effective POMs. Every module's resolution reads through one `Metadata`, so an
//! artifact many modules depend on is parsed once.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use super::effective_pom::{EffectivePom, Reading};
use super::gradle_module::{self, Variant};
use super::pom::{self, Pom};
use super::{Coordinates, Problem, Store, StoredFile};

pub struct Metadata<'s> {
    store: &'s Store,
    /// Files by coordinates and extension: absent, read, or why they cannot be read.
    files: Cache<(Coordinates, &'static str), FileRead>,
    poms: Cache<Coordinates, Result<Rc<SourcedPom>, Problem>>,
    modules: Cache<Coordinates, Result<Rc<[Variant]>, Problem>>,
    /// Effective POMs by their coordinates and the JDK version their profiles were judged for.
    effective: Cache<EffectiveKey, Result<Rc<EffectivePom>, Vec<Problem>>>,
    /// The effective POMs being read, which a POM importing itself must read anew.
    reading: RefCell<HashSet<EffectiveKey>>,
}

type Cache<K, V> = RefCell<HashMap<K, V>>;

pub(super) struct SourcedPom {
    pub pom: Pom,
    pub path: PathBuf,
}

/// A metadata file in the store: `Ok(None)` when no repository holds it.
pub type FileRead = Result<Option<Rc<StoredFile>>, Problem>;

type EffectiveKey = (Coordinates, String);

impl<'s> Metadata<'s> {
    pub fn new(store: &'s Store) -> Self {
        Self {
            store,
            files: RefCell::default(),
            poms: RefCell::default(),
            modules: RefCell::default(),
            effective: RefCell::default(),
            reading: RefCell::default(),
        }
    }

    /// The file of `coordinates` with `extension`, when the store holds it.
    pub fn file(&self, coordinates: &Coordinates, extension: &'static str) -> FileRead {
        let key = (coordinates.clone(), extension);
        if let Some(file) = self.files.borrow().get(&key) {
            return file.clone();
        }
        let file = self
            .store
            .read(coordinates, extension)
            .map(|file| file.map(Rc::new));
        self.files.borrow_mut().insert(key, file.clone());
        file
    }

    /// The raw POM published under `coordinates`, or why it cannot be read.
    pub(super) fn pom(&self, coordinates: &Coordinates) -> Result<Rc<SourcedPom>, Problem> {
        if let Some(pom) = self.poms.borrow().get(coordinates) {
            return pom.clone();
        }
        let pom = self.file(coordinates, "pom").and_then(|file| {
            let file = file.ok_or_else(|| missing(coordinates, "POM"))?;
            pom::parse(&file.text, &coordinates.group, &coordinates.artifact)
                .map(|pom| {
                    Rc::new(SourcedPom {
                        pom,
                        path: file.path.clone(),
                    })
                })
                .map_err(|error| {
                    Problem::in_file(
                        &file.path,
                        error.position,
                        format!(
                            "Unable to parse the POM of {}: {}",
                            coordinates.pretty(None),
                            error.message
                        ),
                    )
                })
        });
        self.poms
            .borrow_mut()
            .insert(coordinates.clone(), pom.clone());
        pom
    }

    /// The variants of the Gradle module metadata `file` published under `coordinates`.
    pub fn variants(
        &self,
        coordinates: &Coordinates,
        file: &StoredFile,
    ) -> Result<Rc<[Variant]>, Problem> {
        if let Some(variants) = self.modules.borrow().get(coordinates) {
            return variants.clone();
        }
        let variants = gradle_module::parse(&file.text)
            .map(Rc::from)
            .map_err(|error| {
                Problem::in_file(
                    &file.path,
                    error.position,
                    format!(
                        "Unable to parse the module metadata of {}: {}",
                        coordinates.pretty(None),
                        error.message
                    ),
                )
            });
        self.modules
            .borrow_mut()
            .insert(coordinates.clone(), variants.clone());
        variants
    }

    /// The effective POM published under `coordinates`, its profiles judged for `jdk_version`.
    pub fn effective_pom(
        &self,
        coordinates: &Coordinates,
        jdk_version: &str,
    ) -> Result<Rc<EffectivePom>, Vec<Problem>> {
        self.shared_effective_pom(coordinates, jdk_version)
            .unwrap_or_else(|| self.read_effective_pom(coordinates, jdk_version))
    }

    /// The effective POM published under `coordinates`, unless it is being read.
    pub(super) fn shared_effective_pom(
        &self,
        coordinates: &Coordinates,
        jdk_version: &str,
    ) -> Option<Result<Rc<EffectivePom>, Vec<Problem>>> {
        let key = (coordinates.clone(), jdk_version.to_string());
        if let Some(effective) = self.effective.borrow().get(&key) {
            return Some(effective.clone());
        }
        if !self.reading.borrow_mut().insert(key.clone()) {
            return None;
        }
        let effective = self.read_effective_pom(coordinates, jdk_version);
        self.reading.borrow_mut().remove(&key);
        self.effective.borrow_mut().insert(key, effective.clone());
        Some(effective)
    }

    fn read_effective_pom(
        &self,
        coordinates: &Coordinates,
        jdk_version: &str,
    ) -> Result<Rc<EffectivePom>, Vec<Problem>> {
        self.pom(coordinates)
            .map_err(|problem| vec![problem])
            .map(|raw| {
                let mut reading = Reading::new(self, jdk_version);
                let pom = reading.effective(&raw.pom, &raw.path, 0);
                Rc::new(EffectivePom {
                    pom,
                    problems: reading.problems,
                    height: reading.deepest,
                })
            })
    }
}

/// The problem of an artifact whose `what` no repository holds.
pub fn missing(coordinates: &Coordinates, what: &str) -> Problem {
    Problem::general(format!(
        "The {what} of {} is in no local repository, and krusty-toolchain does not download artifacts",
        coordinates.pretty(None)
    ))
}
