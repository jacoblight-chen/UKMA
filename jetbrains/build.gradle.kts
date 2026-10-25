// UKMA JetBrains plugin — Gradle build (scaffold).
//
// Honest boundary: this build is NOT executed by the repository's acceptance
// gates. It requires the IntelliJ Platform SDK and the Gradle toolchain,
// which this workspace does not provision. Run it on a machine with the
// SDK to produce the distributable .zip (see README.md).

plugins {
    kotlin("jvm") version "1.9.24"
    id("org.jetbrains.intellij") version "1.17.4"
}

repositories {
    mavenCentral()
}

// The plugin is a thin client: it talks to the UKMA server over REST and
// carries no analysis / indexing / model logic.
dependencies {
    implementation("org.jetbrains.kotlin:kotlin-stdlib:1.9.24")
    implementation("org.json:json:20240302")
}

intellij {
    version.set("2024.1")
    type.set("IC") // IntelliJ Community
    plugins listOf()
}

kotlin {
    jvmToolchain(17)
}

// Produce the plugin archive under build/distributions/ukma-jetbrains.zip.
tasks.buildPlugin {
    tasks.named<Copy>("copyResources") {
        duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    }
}