import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "rocks.spotifast.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "rocks.spotifast.android"
        minSdk = 24
        targetSdk = 35
        versionCode = 1
        versionName = "0.13.3"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Sideloadable test builds: sign with the debug key.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }

    sourceSets {
        getByName("main") {
            // The Rust cdylib per ABI, and the generated UniFFI bindings.
            jniLibs.srcDir(layout.buildDirectory.dir("generated/rust/jniLibs"))
            java.srcDir(layout.buildDirectory.dir("generated/uniffi/kotlin"))
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

// ---------------------------------------------------------------------------
// Rust cdylib + UniFFI bindings.
//
// cargo-ndk builds the core for each ABI into `jniLibs/`; uniffi-bindgen
// turns the compiled library's metadata into Kotlin bindings, generated ahead
// of `compileKotlin`. This replaces the org.mozilla.rust-android-gradle
// plugin, which has not kept up with AGP 8.x: the same two commands, driven
// directly and transparently.
// ---------------------------------------------------------------------------

val rustAbis = listOf("arm64-v8a", "armeabi-v7a", "x86_64")
val rustRoot = rootProject.projectDir.parentFile

val cargoNdkBuild = tasks.register<Exec>("cargoNdkBuild") {
    description = "Builds the spotifast-android cdylib for each ABI into jniLibs."
    val jniLibs = layout.buildDirectory.dir("generated/rust/jniLibs").get().asFile
    val args = mutableListOf("cargo", "ndk")
    rustAbis.forEach { args += listOf("-t", it) }
    args += listOf(
        "-o", jniLibs.absolutePath,
        "build", "-p", "spotifast-android",
        "--profile", "release-android",
    )
    commandLine(args)
    workingDir = rustRoot
    inputs.dir(rustRoot.resolve("core/src"))
    inputs.dir(rustRoot.resolve("android/rust/src"))
    outputs.dir(jniLibs)
}

val uniffiBindgen = tasks.register<Exec>("uniffiBindgen") {
    description = "Generates Kotlin bindings from the compiled cdylib."
    dependsOn(cargoNdkBuild)
    val outDir = layout.buildDirectory.dir("generated/uniffi/kotlin").get().asFile
    val lib = layout.buildDirectory
        .file("generated/rust/jniLibs/arm64-v8a/libspotifast_android.so")
        .get()
        .asFile
    commandLine(
        "uniffi-bindgen", "generate",
        "--library", lib.absolutePath,
        "--language", "kotlin",
        "--out-dir", outDir.absolutePath,
        "--no-format",
    )
    inputs.file(lib)
    outputs.dir(outDir)
}

// Compile the generated bindings with the app's Kotlin.
tasks.named("preBuild") {
    dependsOn(uniffiBindgen)
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.security.crypto)
    implementation(libs.androidx.browser)
    // The UniFFI-generated bindings call through JNA.
    implementation(libs.jna)
    debugImplementation(libs.androidx.compose.ui.tooling)
}
