plugins {
    `java-library`
}

group = "io.github.taskcage"
version = "0.1.0-SNAPSHOT"

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(17)
    }
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 17
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("com.fasterxml.jackson.core:jackson-databind:2.19.1")
    testImplementation(platform("org.junit:junit-bom:5.11.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
}

val e2eTest by sourceSets.creating {
    java.srcDir("src/e2eTest/java")
    resources.srcDir("src/e2eTest/resources")
    compileClasspath += sourceSets.main.get().output
    runtimeClasspath += output + compileClasspath
}

val ffmpegE2eTest by sourceSets.creating {
    java.srcDir("src/ffmpegE2eTest/java")
    resources.srcDir("src/ffmpegE2eTest/resources")
    compileClasspath += sourceSets.main.get().output
    runtimeClasspath += output + compileClasspath
}

configurations[e2eTest.implementationConfigurationName].extendsFrom(configurations.testImplementation.get())
configurations[e2eTest.runtimeOnlyConfigurationName].extendsFrom(configurations.testRuntimeOnly.get())
configurations[ffmpegE2eTest.implementationConfigurationName].extendsFrom(configurations.testImplementation.get())
configurations[ffmpegE2eTest.runtimeOnlyConfigurationName].extendsFrom(configurations.testRuntimeOnly.get())

tasks.test {
    useJUnitPlatform()
}

tasks.register<Test>("e2eTest") {
    group = "verification"
    description = "Runs core API tests against a real TaskCage daemon on Linux."
    testClassesDirs = e2eTest.output.classesDirs
    classpath = e2eTest.runtimeClasspath
    useJUnitPlatform()
    doFirst {
        require(!System.getenv("TASKCAGE_SOCKET").isNullOrBlank()) {
            "e2eTest requires TASKCAGE_SOCKET to point to a running TaskCage daemon"
        }
        require(!System.getenv("TASKCAGE_GHOST_TREE").isNullOrBlank()) {
            "e2eTest requires TASKCAGE_GHOST_TREE to point to the ghost-tree fixture"
        }
        require(!System.getenv("TASKCAGE_OUTPUT_FLOOD").isNullOrBlank()) {
            "e2eTest requires TASKCAGE_OUTPUT_FLOOD to point to the output-flood fixture"
        }
    }
}

tasks.register<Test>("ffmpegE2eTest") {
    group = "verification"
    description = "Runs the FFmpeg reference workflow against an installed TaskCage daemon on Linux."
    testClassesDirs = ffmpegE2eTest.output.classesDirs
    classpath = ffmpegE2eTest.runtimeClasspath
    useJUnitPlatform()
    doFirst {
        listOf(
            "TASKCAGE_SOCKET",
            "TASKCAGE_FFMPEG",
            "TASKCAGE_FFMPEG_TREE",
            "TASKCAGE_FFMPEG_WORK_DIR",
        ).forEach { name ->
            require(!System.getenv(name).isNullOrBlank()) {
                "ffmpegE2eTest requires $name"
            }
        }
    }
}
