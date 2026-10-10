//! A module's classes as the KLIB backend writes them, read back through the ordinary KLIB reader.
//!
//! The source is analyzed exactly as every Native test analyzes it ([`common::native_analysis`]),
//! compiled with `KlibBackend`, written as a library, and opened with `KlibArchive`. Each fragment
//! is decoded with `parse_package_fragment_checked`, and the test compares the whole decoded class
//! surface, in fragment order, as text.

use std::fmt::Write as _;

use krusty::diag::DiagSink;
use krusty::klib::backend::KlibBackend;
use krusty::klib::write::{KlibPlatform, KlibStamp};
use krusty::klib::KlibArchive;
use krusty::metadata::semantic::{parse_package_fragment_checked, KotlinPackage, KotlinType};

use crate::common;

const CLASSES: &str = r#"package shapes

annotation class Marker

@Marker
open class Shape(val name: String, var sides: Int = 0) {
    constructor(name: String, a: Int, b: Int) : this(name, a + b)

    open fun area(): Double = 0.0

    fun describe(prefix: String): String = prefix + name

    class Nested(val depth: Int)

    inner class Inner {
        fun owner(): String = name
    }

    companion object {
        const val UNIT: Int = 1
        fun origin(): Shape = Shape("origin")
    }
}

interface Measured {
    val size: Long
    fun measure(scale: Int): Long
}

abstract class Polygon<T : Comparable<T>>(val corners: List<T>) : Shape("polygon"), Measured {
    abstract fun largest(): T
}

enum class Color(val rgb: Int) {
    RED(0xff0000),
    GREEN(0x00ff00);

    fun hex(): String = rgb.toString(16)
}

sealed class Outcome {
    data class Done(val value: Int, @Marker val label: String) : Outcome()
    object Pending : Outcome()
}

object Registry {
    const val LIMIT: Long = 10L
    @Marker var count: Int = 0
}
"#;

/// The library the KLIB backend writes for `sources`, opened through the ordinary reader, with each
/// fragment decoded in the archive's order. `None` when this build has no stdlib to analyze against.
fn written_packages(sources: &[(&str, &str)]) -> Option<(Vec<String>, Vec<KotlinPackage>)> {
    let mut diags = DiagSink::new();
    let (analysis, stems) = common::native_analysis(sources, &mut diags)?;
    let backend = KlibBackend::new(
        KlibPlatform::Native,
        KlibStamp::new(
            krusty::kotlin_version::KotlinVersion::V2_4_20,
            krusty::language_version::LanguageVersion::V2_4,
        ),
    );
    let entries = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "lib", &mut diags);
    let messages = diags
        .diags
        .iter()
        .map(|diagnostic| diagnostic.msg.clone())
        .collect::<Vec<_>>();
    if !messages.is_empty() {
        return Some((messages, Vec::new()));
    }
    let root = std::env::temp_dir().join(format!(
        "krusty-klib-classes-{}-{}",
        std::process::id(),
        stems.join("-")
    ));
    let _ = std::fs::remove_dir_all(&root);
    for (entry, bytes) in &entries {
        let path = root.join(entry);
        std::fs::create_dir_all(path.parent().expect("an entry has a directory"))
            .expect("create the entry's directory");
        std::fs::write(&path, bytes).expect("write the entry");
    }
    let archive = KlibArchive::open(&root).expect("open the written library");
    let packages = archive
        .package_fragments()
        .iter()
        .map(|fragment| {
            let bytes = archive.read(&fragment.entry).expect("read a fragment");
            parse_package_fragment_checked(&bytes).expect("decode a fragment")
        })
        .collect();
    std::fs::remove_dir_all(&root).expect("remove the library");
    Some((Vec::new(), packages))
}

/// A decoded type as Kotlin spells it, with the internal (`/`-separated) class name.
fn render(ty: &KotlinType) -> String {
    match ty {
        KotlinType::Class {
            internal,
            args,
            nullable,
            ..
        } => {
            let mut out = internal.clone();
            if !args.is_empty() {
                out.push('<');
                out.push_str(&types(args));
                out.push('>');
            }
            if *nullable {
                out.push('?');
            }
            out
        }
        // A reference names the declaration it binds to by identity, not only by spelling.
        KotlinType::Param { name, id, nullable } => {
            format!("{name}#{}{}", id.0, if *nullable { "?" } else { "" })
        }
        KotlinType::InProjection(inner) => format!("in {}", render(inner)),
        KotlinType::OutProjection(inner) => format!("out {}", render(inner)),
        KotlinType::Star => "*".to_string(),
    }
}

fn types(tys: &[KotlinType]) -> String {
    tys.iter().map(render).collect::<Vec<_>>().join(", ")
}

/// Every class of `package` in fragment order, one declaration per line.
fn surface(package: &KotlinPackage) -> String {
    let mut out = String::new();
    for name in &package.class_order {
        let class = &package.classes[name];
        writeln!(
            out,
            "class {name} {:?} {:?} {:?} nested={} : {}",
            class.kind,
            class.modality,
            class.visibility,
            class.is_nested,
            types(&class.supertype_tys)
        )
        .unwrap();
        for parameter in &class.type_params {
            writeln!(
                out,
                "  type {}#{} : {}",
                parameter.name,
                parameter.id.0,
                types(&parameter.bounds)
            )
            .unwrap();
        }
        for annotation in &class.annotations {
            writeln!(out, "  @{}", annotation.identity.render()).unwrap();
        }
        for constructor in &class.constructors {
            writeln!(
                out,
                "  constructor {:?} ({}) defaults={:?} {:?}",
                constructor.param_names,
                types(&constructor.params),
                constructor.param_defaults,
                constructor.visibility
            )
            .unwrap();
        }
        for member in &class.members {
            let kind = if member.is_property { "val" } else { "fun" };
            writeln!(
                out,
                "  {kind} {}({}): {} abstract={} const={:?} annotations={:?}",
                member.name,
                types(&member.params),
                render(&member.ret),
                member.is_abstract,
                member.constant,
                member
                    .annotations
                    .iter()
                    .map(|annotation| annotation.render())
                    .collect::<Vec<_>>()
            )
            .unwrap();
        }
        if let Some(companion) = &class.companion_name {
            writeln!(out, "  companion {companion}").unwrap();
        }
        if !class.enum_entries.is_empty() {
            writeln!(out, "  entries {:?}", class.enum_entries).unwrap();
        }
        if !class.sealed_subclasses.is_empty() {
            writeln!(out, "  sealed {:?}", class.sealed_subclasses).unwrap();
        }
    }
    out
}

#[test]
fn every_supported_class_shape_reads_back_in_declaration_order() {
    let Some((messages, packages)) = written_packages(&[("shapes", CLASSES)]) else {
        return;
    };
    assert_eq!(messages, Vec::<String>::new());
    assert_eq!(packages.len(), 1);
    assert_eq!(surface(&packages[0]), EXPECTED_SURFACE);
}

#[test]
fn a_value_class_is_declined_by_name_and_writes_nothing() {
    let Some((messages, packages)) = written_packages(&[(
        "boxed",
        "package boxed\n@JvmInline value class Meters(val value: Int)\n",
    )]) else {
        return;
    };
    assert_eq!(
        messages,
        ["krusty: the KLIB writer does not support value classes yet"]
    );
    assert_eq!(packages.len(), 0);
}

/// The class surface of [`CLASSES`]: classes in pre-order (each nested class after its owner, siblings
/// in source order), each with its declared members.
const EXPECTED_SURFACE: &str = r#"class shapes/Marker Annotation Final Public nested=false : kotlin/Annotation
  constructor [] () defaults=[] Public
class shapes/Shape Class Open Public nested=false : kotlin/Any
  @shapes/Marker
  constructor ["name", "sides"] (kotlin/String, kotlin/Int) defaults=[false, true] Public
  constructor ["name", "a", "b"] (kotlin/String, kotlin/Int, kotlin/Int) defaults=[false, false, false] Public
  fun area(): kotlin/Double abstract=false const=None annotations=[]
  fun describe(kotlin/String): kotlin/String abstract=false const=None annotations=[]
  val name(): kotlin/String abstract=false const=None annotations=[]
  val sides(): kotlin/Int abstract=false const=None annotations=[]
  companion Companion
class shapes/Shape.Nested Class Final Public nested=true : kotlin/Any
  constructor ["depth"] (kotlin/Int) defaults=[false] Public
  val depth(): kotlin/Int abstract=false const=None annotations=[]
class shapes/Shape.Inner Class Final Public nested=true : kotlin/Any
  constructor [] () defaults=[] Public
  fun owner(): kotlin/String abstract=false const=None annotations=[]
class shapes/Shape.Companion Object Final Public nested=true : kotlin/Any
  constructor [] () defaults=[] Private
  fun origin(): shapes/Shape abstract=false const=None annotations=[]
  val UNIT(): kotlin/Int abstract=false const=Some(Int(1)) annotations=[]
class shapes/Measured Interface Abstract Public nested=false : kotlin/Any
  fun measure(kotlin/Int): kotlin/Long abstract=true const=None annotations=[]
  val size(): kotlin/Long abstract=true const=None annotations=[]
class shapes/Polygon Class Abstract Public nested=false : shapes/Shape, shapes/Measured
  type T#0 : kotlin/Comparable<T#0>
  constructor ["corners"] (kotlin/collections/List<T#0>) defaults=[false] Public
  fun largest(): T#0 abstract=true const=None annotations=[]
  val corners(): kotlin/collections/List<T#0> abstract=false const=None annotations=[]
class shapes/Color Enum Final Public nested=false : kotlin/Enum<shapes/Color>
  constructor ["rgb"] (kotlin/Int) defaults=[false] Private
  fun hex(): kotlin/String abstract=false const=None annotations=[]
  val rgb(): kotlin/Int abstract=false const=None annotations=[]
  entries ["RED", "GREEN"]
class shapes/Outcome Class Sealed Public nested=false : kotlin/Any
  constructor [] () defaults=[] Protected
  sealed ["shapes/Outcome.Done", "shapes/Outcome.Pending"]
class shapes/Outcome.Done Class Final Public nested=true : shapes/Outcome
  constructor ["value", "label"] (kotlin/Int, kotlin/String) defaults=[false, false] Public
  fun component1(): kotlin/Int abstract=false const=None annotations=[]
  fun component2(): kotlin/String abstract=false const=None annotations=[]
  fun copy(kotlin/Int, kotlin/String): shapes/Outcome.Done abstract=false const=None annotations=[]
  fun equals(kotlin/Any?): kotlin/Boolean abstract=false const=None annotations=[]
  fun hashCode(): kotlin/Int abstract=false const=None annotations=[]
  fun toString(): kotlin/String abstract=false const=None annotations=[]
  val value(): kotlin/Int abstract=false const=None annotations=[]
  val label(): kotlin/String abstract=false const=None annotations=[]
class shapes/Outcome.Pending Object Final Public nested=true : shapes/Outcome
  constructor [] () defaults=[] Private
class shapes/Registry Object Final Public nested=false : kotlin/Any
  constructor [] () defaults=[] Private
  val LIMIT(): kotlin/Long abstract=false const=Some(Long(10)) annotations=[]
  val count(): kotlin/Int abstract=false const=None annotations=["shapes/Marker"]
"#;
