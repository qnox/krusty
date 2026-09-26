//! kotlinc's member ORDER in the class file, which follows its IR declaration list after lowering:
//!
//! - a file facade lists the file's declarations in source order, each top-level property's
//!   accessors at the property's position (JvmPropertiesLowering replaces the property in place);
//! - lifted local functions follow the declared members in LocalDeclarationPopupLowering's order:
//!   bodies finish postfix, and each appends its own local functions in source order, so a nested
//!   local function precedes the one that declares it; indy lambda methods come after all of them;
//! - an enum's declared members precede `values`/`valueOf`/`getEntries`; its lifted local functions
//!   come next, then EnumClassLowering's `$values`, then the indy lambda methods;
//! - a synthetic `access$…` bridge (SyntheticAccessorLowering) is appended after every declared and
//!   lifted member;
//! - a class's lexical captures and outer instance (LocalDeclarationsLowering, InnerClassesLowering)
//!   are appended after its declared fields.
//! - members an interface inherits from sibling supertypes (its `access$…$jd` bridges, its
//!   `DefaultImpls` forwarders, an implementing class's default-method forwarders) follow the
//!   supertypes' declaration order at every level of the hierarchy.
//!
//! Each case asserts the complete member list of every class kotlinc emits — names and descriptors
//! in order — against the reference compiler. The fixtures use neutral names only.
use super::common;

/// Compile `src` with kotlinc and with krusty; for each class kotlinc emits, the `javap -p -s`
/// member lines of both, in emission order.
fn member_lists(stem: &str, src: &str) -> Vec<(String, Vec<String>, Vec<String>)> {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    std::fs::create_dir_all(&krusty_dir).expect("krusty output directory");
    let source = dir.join(format!("{stem}.kt"));
    std::fs::write(&source, src).expect("write fixture");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let classes =
        common::compile_in_process_metadata_cp_module_target(src, stem, &[], "main", None)
            .expect("krusty compiles the fixture");
    for (internal, bytes) in &classes {
        std::fs::write(krusty_dir.join(format!("{internal}.class")), bytes)
            .expect("write krusty class");
    }
    let mut reference_classes: Vec<String> = std::fs::read_dir(&reference_dir)
        .expect("reference classes")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".class").map(str::to_string)
        })
        .filter(|name| name != "META-INF")
        .collect();
    reference_classes.sort();
    let krusty_classes: std::collections::BTreeSet<&str> = classes
        .iter()
        .map(|(internal, _)| internal.as_str())
        .collect();
    assert_eq!(
        krusty_classes,
        reference_classes.iter().map(String::as_str).collect(),
        "class set"
    );
    let mut lists = Vec::new();
    for class in reference_classes {
        let members = |root: &std::path::Path| {
            let out = common::javap(&["-p", "-s", "-cp", &root.to_string_lossy(), &class])
                .expect("javap reads the class");
            // Each declaration row is followed by its `descriptor:` row; keep the member's name and
            // descriptor, not its modifiers, which are not what this file is about.
            let lines: Vec<&str> = out.lines().map(str::trim).collect();
            lines
                .windows(2)
                .filter_map(|pair| {
                    let descriptor = pair[1].strip_prefix("descriptor: ")?;
                    let declaration = pair[0].trim_end_matches(';');
                    let name = declaration.split('(').next()?.split_whitespace().last()?;
                    Some(format!("{name} {descriptor}"))
                })
                .collect::<Vec<_>>()
        };
        lists.push((class.clone(), members(&reference_dir), members(&krusty_dir)));
    }
    let _ = std::fs::remove_dir_all(&dir);
    lists
}

fn assert_same_member_order(stem: &str, src: &str) {
    for (class, reference, krusty) in member_lists(stem, src) {
        assert_eq!(krusty, reference, "member order of {class}");
    }
}

#[test]
fn a_facade_places_property_accessors_at_their_source_position() {
    assert_same_member_order(
        "FacadeOrder",
        "class Crate(val weight: Int)\n\
         var tally = 0\n\
         fun first(): Int = tally\n\
         val Crate.heavy: Boolean get() = weight > 10\n\
         val label: String = \"crate\"\n\
         var gauge: Int = 1\n\
         \x20   get() = field + 1\n\
         \x20   set(value) { field = value }\n\
         fun second(): String = label + gauge + Crate(3).heavy\n",
    );
}

#[test]
fn a_custom_getter_stays_before_its_implicit_setter() {
    assert_same_member_order(
        "MixedAccessorOrder",
        "class Meter {\n\
         \x20   var reading: Int = 1\n\
         \x20       get() = field + 1\n\
         \x20   fun marker(): Int = reading\n\
         }\n",
    );
}

#[test]
fn lifted_local_functions_finish_postfix_before_lambdas() {
    assert_same_member_order(
        "LiftedOrder",
        "fun apply(block: () -> Int): Int = block()\n\
         fun outer(seed: Int): Int {\n\
         \x20   fun middle(step: Int): Int {\n\
         \x20       fun inner(): Int = step + seed\n\
         \x20       return inner() + apply { step }\n\
         \x20   }\n\
         \x20   fun sibling(): Int = apply { seed } + middle(1)\n\
         \x20   return sibling() + apply { middle(2) }\n\
         }\n\
         fun later(): Int {\n\
         \x20   fun helper(): Int = apply { 3 }\n\
         \x20   return helper()\n\
         }\n",
    );
}

#[test]
fn a_facade_appends_private_function_bridges_after_its_members() {
    assert_same_member_order(
        "BridgeOrder",
        "private fun hidden(value: Int): Int = value * 2\n\
         class Probe { fun read(): Int = hidden(4) }\n\
         fun visible(): Int = Probe().read()\n",
    );
}

#[test]
fn captured_and_outer_fields_follow_declared_fields() {
    assert_same_member_order(
        "CaptureOrder",
        "class Shell(val base: Int) {\n\
         \x20   inner class Core(val extra: Int) { fun total(): Int = base + extra }\n\
         }\n\
         fun build(seed: Int): Any {\n\
         \x20   class Local(val own: Int) { fun sum(): Int = own + seed }\n\
         \x20   return Local(1).sum() + Shell(2).Core(3).total()\n\
         }\n",
    );
}

#[test]
fn an_enum_places_local_functions_before_values_array_and_lambdas_after_it() {
    assert_same_member_order(
        "EnumLifted",
        "enum class Mode {\n\
         \x20   ON, OFF;\n\
         \x20   fun level(): Int {\n\
         \x20       fun base() = 2\n\
         \x20       val step = { base() + 1 }\n\
         \x20       return step()\n\
         \x20   }\n\
         \x20   fun spare() = 5\n\
         }\n",
    );
}

#[test]
fn inherited_interface_members_follow_sibling_supertypes_in_declaration_order() {
    assert_same_member_order(
        "InheritedSiblings",
        "interface Left { fun left() = \"l\" }\n\
         interface Right { fun right() = \"r\" }\n\
         interface Both : Left, Right\n\
         interface Child : Both\n\
         class Leaf : Child\n",
    );
}
