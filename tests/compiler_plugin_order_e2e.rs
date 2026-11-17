//! kotlinc's `-Xcompiler-plugin-order=<before>><after>`, which orders the compiler plugins a
//! compilation loads. Each fixture compiles the same sources with the same switches through kotlinc
//! and through the krusty binary: a valid order compiles to the same classes, and a constraint
//! kotlinc cannot parse or a cycle among loaded plugins is kotlinc's own error.

use super::common;
use super::compiler_plugin_fixture::{
    assert_same_classes_and_box, kotlinc_plugin_jar, PluginFixture,
};

const ALLOPEN_ID: &str = "org.jetbrains.kotlin.allopen";
const NOARG_ID: &str = "org.jetbrains.kotlin.noarg";

const SOURCE: &str = r#"annotation class Open
annotation class NoArg

@Open
@NoArg
class Entity(val name: String) {
    fun describe() = "entity:$name"
}

class Special : Entity("special") {
    override fun describe() = "special"
}

fun box(): String {
    val created = Entity::class.java.getDeclaredConstructor().newInstance()
    if (created.describe() != "entity:null") return "Fail no-arg: ${created.describe()}"
    if (Special().describe() != "special") return "Fail all-open"
    return "OK"
}
"#;

/// The all-open and no-arg plugins, loaded by `-Xplugin` in the given order and configured for the
/// fixture's annotations, then the order constraints.
fn switches(allopen_first: bool, order: &[&str]) -> Vec<String> {
    let allopen = kotlinc_plugin_jar("allopen-compiler-plugin.jar");
    let noarg = kotlinc_plugin_jar("noarg-compiler-plugin.jar");
    let jars = if allopen_first {
        [allopen, noarg]
    } else {
        [noarg, allopen]
    };
    let mut switches = jars
        .iter()
        .map(|jar| format!("-Xplugin={}", jar.display()))
        .collect::<Vec<_>>();
    for option in [
        format!("plugin:{ALLOPEN_ID}:annotation=Open"),
        format!("plugin:{NOARG_ID}:annotation=NoArg"),
    ] {
        switches.extend(["-P".to_string(), option]);
    }
    switches.extend(
        order
            .iter()
            .map(|constraint| format!("-Xcompiler-plugin-order={constraint}")),
    );
    switches
}

#[test]
fn an_order_between_loaded_plugins_compiles_as_kotlinc_does() {
    let stdlib = vec![common::stdlib_jar()];
    let before = format!("{NOARG_ID}>{ALLOPEN_ID}");
    // Metro's Gradle plugin adds this constraint; neither plugin is loaded here.
    let unloaded = "dev.zacsweers.metro.compiler>androidx.compose.compiler.plugins.kotlin";
    for (name, allopen_first, order) in [
        ("noarg-first", true, vec![before.as_str()]),
        ("unloaded", false, vec![unloaded]),
        ("both", true, vec![unloaded, before.as_str()]),
    ] {
        let fixture = PluginFixture::new(&format!("plugin-order-{name}"));
        let sources = [("Entity.kt", SOURCE)];
        let switches = switches(allopen_first, &order);
        let reference = fixture.kotlinc("main", &sources, &[], &switches);
        let krusty = fixture.krusty("main", &sources, &[], &switches);
        assert_same_classes_and_box(&reference, &krusty, &stdlib);
    }
}

#[test]
fn a_constraint_kotlinc_cannot_parse_is_its_error() {
    for (name, constraint) in [("bare", "foo"), ("chained", "a>b>c"), ("empty", "")] {
        let fixture = PluginFixture::new(&format!("plugin-order-unparsed-{name}"));
        let sources = [("Entity.kt", SOURCE)];
        let switches = switches(true, &[constraint]);
        let expected = fixture.kotlinc_rejects("main", &sources, &[], &switches);
        let actual = fixture.krusty_rejects("main", &sources, &[], &switches);
        assert_eq!(actual, expected, "{constraint:?}");
        assert_eq!(
            expected,
            format!("error: could not parse plugin order constraint: {constraint}\n")
        );
    }
}

#[test]
fn a_cycle_among_loaded_plugins_is_kotlincs_error() {
    let forward = format!("{ALLOPEN_ID}>{NOARG_ID}");
    let backward = format!(" {NOARG_ID} > {ALLOPEN_ID} ");
    let own = format!("{ALLOPEN_ID}>{ALLOPEN_ID}");
    let rendered = |before: &str, after: &str, raw: &str| {
        format!("PluginOrderConstraint(before={before}, after={after}, rawArgument={raw})")
    };
    let both = [
        rendered(ALLOPEN_ID, NOARG_ID, &forward),
        rendered(NOARG_ID, ALLOPEN_ID, &backward),
    ]
    .join(", ");
    let unrelated_and_own = [
        rendered("x", "y", "x>y"),
        rendered(ALLOPEN_ID, ALLOPEN_ID, &own),
    ]
    .join(", ");
    // The plugin reported is the first one the depth-first walk over the load order reaches again.
    for (name, allopen_first, order, reported, constraints) in [
        (
            "allopen-first",
            true,
            vec![forward.as_str(), backward.as_str()],
            ALLOPEN_ID,
            &both,
        ),
        (
            "noarg-first",
            false,
            vec![forward.as_str(), backward.as_str()],
            NOARG_ID,
            &both,
        ),
        (
            "self",
            true,
            vec!["x>y", own.as_str()],
            ALLOPEN_ID,
            &unrelated_and_own,
        ),
    ] {
        let fixture = PluginFixture::new(&format!("plugin-order-cycle-{name}"));
        let sources = [("Entity.kt", SOURCE)];
        let switches = switches(allopen_first, &order);
        let expected = fixture.kotlinc_rejects("main", &sources, &[], &switches);
        let actual = fixture.krusty_rejects("main", &sources, &[], &switches);
        assert_eq!(actual, expected, "{name}");
        assert_eq!(
            expected,
            format!("error: compiler plugin '{reported}' is part of an constraint cycle: {constraints}\n"),
            "{name}"
        );
    }
}

#[test]
fn a_cycle_is_reported_before_a_plugins_options_are_processed() {
    let forward = format!("{ALLOPEN_ID}>{NOARG_ID}");
    let backward = format!("{NOARG_ID}>{ALLOPEN_ID}");
    let constraints = [
        format!(
            "PluginOrderConstraint(before={ALLOPEN_ID}, after={NOARG_ID}, rawArgument={forward})"
        ),
        format!(
            "PluginOrderConstraint(before={NOARG_ID}, after={ALLOPEN_ID}, rawArgument={backward})"
        ),
    ]
    .join(", ");
    let fixture = PluginFixture::new("plugin-order-cycle-with-bad-option");
    let sources = [("Entity.kt", SOURCE)];
    // The all-open plugin declares no `unknown` option; kotlinc reports that option only once the
    // plugins are ordered, and the order has no solution.
    let mut switches = switches(true, &[forward.as_str(), backward.as_str()]);
    switches.extend(["-P".to_string(), format!("plugin:{ALLOPEN_ID}:unknown=1")]);
    let expected = fixture.kotlinc_rejects("main", &sources, &[], &switches);
    let actual = fixture.krusty_rejects("main", &sources, &[], &switches);
    assert_eq!(actual, expected);
    assert_eq!(
        expected,
        format!(
            "error: compiler plugin '{ALLOPEN_ID}' is part of an constraint cycle: {constraints}\n"
        )
    );
}
