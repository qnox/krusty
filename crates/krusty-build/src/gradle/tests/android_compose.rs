//! A small Android application with Jetpack Compose and `@Parcelize`, built by the Android Gradle
//! plugin's built-in Kotlin with krusty as the compiler: which of its modules krusty compiles
//! itself, and which it hands to kotlinc because a native compiler-plugin pass does not cover what
//! the module uses yet.

use super::*;

const AGP_VERSION: &str = "9.4.1";

/// `:model` holds the `@Parcelize` state and its `Bundle` helpers, which krusty compiles with the
/// native Parcelize pass. `:app` holds the Compose screen, whose Compose compiler plugin pass is
/// partial: the Kotlin task for `:app` is handed to kotlinc, and its classes are the ones a
/// kotlinc-only build writes.
///
/// Needs `ANDROID_HOME` and Gradle 9.6 or newer (AGP 9.4), so CI runs it on those lanes.
#[test]
#[ignore = "downloads Gradle, the Android Gradle plugin, the Kotlin Gradle plugin and Compose"]
fn android_compose_app_is_compiled_by_krusty_with_a_handoff_for_what_it_refuses() {
    assert!(
        std::env::var_os("ANDROID_HOME").is_some(),
        "ANDROID_HOME must name an Android SDK"
    );
    let fixture = IntegrationFixture::new("krusty-android-compose", write_android_compose_build);
    let (root, log) = (&fixture.root, &fixture.log);
    let model_classes =
        root.join("model/build/intermediates/built_in_kotlinc/debug/compileDebugKotlin/classes");
    let app_classes =
        root.join("app/build/intermediates/built_in_kotlinc/debug/compileDebugKotlin/classes");

    let output = fixture
        .build()
        .tasks([":app:compileDebugKotlin"])
        .run_output()
        .unwrap_or_else(|error| panic!("the Compose app builds: {error}"));

    // `:model` is krusty's: no hand-off, and the Parcelize pass wrote the `Creator`.
    assert!(!output.contains("task ':model:"), "{output}");
    let model = class_files_under(&model_classes);
    assert!(
        model
            .keys()
            .any(|name| name.ends_with("CounterState$Creator.class")),
        "{:?}",
        model.keys().collect::<Vec<_>>()
    );

    // `:app` is refused by the Compose pass, so its Kotlin task is kotlinc's, once, with every
    // reason logged.
    let header = "krusty: task ':app:compileDebugKotlin' is left to kotlinc: krusty does not \
                  implement everything the module uses";
    let lines = output.lines().collect::<Vec<_>>();
    let at = lines
        .iter()
        .position(|line| *line == header)
        .unwrap_or_else(|| panic!("{header}\n{output}"));
    let reasons = lines[at + 1..]
        .iter()
        .take_while(|line| line.starts_with("  "))
        .map(|line| line.trim_start())
        .collect::<Vec<_>>();
    let source = format!(
        "{}/app/src/main/kotlin/sample/app/MainActivity.kt",
        root.display()
    );
    let expected = [("27:1", "a lambda in a composable function")]
        .map(|(at, what)| {
            format!(
            "{source}:{at}: error: krusty's Compose compiler plugin does not support {what} yet; \
             compile this module with kotlinc"
        )
        })
        .to_vec();
    assert_eq!(reasons, expected, "{output}");
    let text = read_invocation_log(log).expect("invocation log");
    assert_eq!(invocations(&text).len(), 2, "{text}");
    let handed = class_files_under(&app_classes);
    assert!(!handed.is_empty(), "{output}");

    // The same project with krusty left out writes the same bytes.
    let _ = std::fs::remove_dir_all(root.join("app/build"));
    let _ = std::fs::remove_dir_all(root.join("model/build"));
    fixture
        .build()
        .property("krusty.negative", "kotlinc-only")
        .tasks([":app:compileDebugKotlin"])
        .run_output()
        .unwrap_or_else(|error| panic!("the same app without krusty: {error}"));
    assert_same_classes(&class_files_under(&app_classes), &handed);
    // krusty's Parcelize pass writes the same bytes for `:model`.
    assert_same_classes(&class_files_under(&model_classes), &model);

    let _ = std::fs::remove_dir_all(root);
}

fn write_android_compose_build(root: &Path, plugin_version: &str, kgp_version: &str) {
    let write = |rel: &str, body: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let body = body
            .replace("PLUGIN_VERSION", plugin_version)
            .replace("KGP_VERSION", kgp_version)
            .replace("AGP_VERSION", AGP_VERSION);
        std::fs::write(path, body).expect("write");
    };
    write(
        "settings.gradle.kts",
        r#"
pluginManagement {
    repositories {
        providers.gradleProperty("krusty.plugin.repository").orNull?.let {
            maven { url = uri(it) }
        }
        google()
        gradlePluginPortal()
        mavenCentral()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "android-compose"
include(":model")
include(":app")
"#,
    );
    // AGP 9 compiles Kotlin through the Kotlin Gradle plugin it finds on the build classpath; the
    // unapplied Kotlin Android plugin pins that version, and the compiler-plugin wrappers share it.
    write(
        "build.gradle.kts",
        r#"
plugins {
    id("com.android.application") version "AGP_VERSION" apply false
    id("com.android.library") version "AGP_VERSION" apply false
    kotlin("android") version "KGP_VERSION" apply false
    kotlin("plugin.compose") version "KGP_VERSION" apply false
    kotlin("plugin.parcelize") version "KGP_VERSION" apply false
    id("dev.krustythecompiler") version "PLUGIN_VERSION" apply false
}
"#,
    );
    write(
        "gradle.properties",
        "android.useAndroidX=true\norg.gradle.jvmargs=-Xmx3g -XX:MaxMetaspaceSize=1g\n",
    );
    write(
        "model/build.gradle.kts",
        r#"
plugins {
    id("com.android.library")
    kotlin("plugin.parcelize")
}

android {
    namespace = "sample.model"
    compileSdk = 36
    defaultConfig { minSdk = 26 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

if (providers.gradleProperty("krusty.negative").orNull != "kotlinc-only") {
    pluginManager.apply("dev.krustythecompiler")
} else {
    // Changes the compile's cache key, so the reference build compiles instead of restoring the
    // krusty build's classes.
    kotlin { compilerOptions { freeCompilerArgs.add("-nowarn") } }
}
"#,
    );
    write(
        "model/src/main/AndroidManifest.xml",
        "<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\" />\n",
    );
    write(
        "model/src/main/kotlin/sample/model/Counter.kt",
        r#"package sample.model

import android.os.Bundle
import android.os.Parcelable
import kotlinx.parcelize.Parcelize

@Parcelize
data class CounterState(val label: String, val count: Int) : Parcelable

private const val KEY = "counter"

fun CounterState.toBundle(): Bundle = Bundle().apply { putParcelable(KEY, this@toBundle) }

@Suppress("DEPRECATION")
fun Bundle.counterState(): CounterState? = getParcelable(KEY)
"#,
    );
    write(
        "app/build.gradle.kts",
        r#"
plugins {
    id("com.android.application")
    kotlin("plugin.compose")
}

android {
    namespace = "sample.app"
    compileSdk = 36
    defaultConfig {
        applicationId = "sample.compose"
        minSdk = 26
        targetSdk = 36
    }
    buildFeatures { compose = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(project(":model"))
    implementation(platform("androidx.compose:compose-bom:2025.10.00"))
    implementation("androidx.activity:activity-compose:1.11.0")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.runtime:runtime")
    implementation("androidx.compose.ui:ui")
}

if (providers.gradleProperty("krusty.negative").orNull != "kotlinc-only") {
    pluginManager.apply("dev.krustythecompiler")
} else {
    kotlin { compilerOptions { freeCompilerArgs.add("-nowarn") } }
}
"#,
    );
    write(
        "app/src/main/AndroidManifest.xml",
        r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
    <application android:label="Compose sample">
        <activity android:name="sample.app.MainActivity" android:exported="true">
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
        </activity>
    </application>
</manifest>
"#,
    );
    write(
        "app/src/main/kotlin/sample/app/MainActivity.kt",
        r#"package sample.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import sample.model.CounterState
import sample.model.counterState

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val restored = savedInstanceState?.counterState() ?: CounterState("Clicks", 0)
        setContent { CounterScreen(restored) }
    }
}

@Composable
fun CounterScreen(initial: CounterState) {
    var count by remember { mutableIntStateOf(initial.count) }
    Column {
        Text("${initial.label}: $count")
        Row {
            Button(onClick = { count++ }) { Text("More") }
            Button(onClick = { count = 0 }) { Text("Reset") }
        }
    }
}
"#,
    );
}

/// The two class sets name the same classes with the same bytes; a difference names the classes.
fn assert_same_classes(
    reference: &std::collections::BTreeMap<String, Vec<u8>>,
    krusty: &std::collections::BTreeMap<String, Vec<u8>>,
) {
    assert_eq!(
        reference.keys().collect::<Vec<_>>(),
        krusty.keys().collect::<Vec<_>>()
    );
    let differing = reference
        .iter()
        .filter(|(name, bytes)| krusty[*name] != **bytes)
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(differing, Vec::<&str>::new());
}
