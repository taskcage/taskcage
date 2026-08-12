plugins {
    `java-library`
}

group = "org.taskcage"
version = "0.1.0"

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(17)
    }
    withJavadocJar()
    withSourcesJar()
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 17
}

repositories {
    mavenCentral()
}

dependencies {
    api("org.taskcage:taskcage-java-sdk:0.2.0")
    testImplementation(platform("org.junit:junit-bom:5.11.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
}

val bindingE2eTest by sourceSets.creating {
    java.srcDir("src/bindingE2eTest/java")
    resources.srcDir("src/bindingE2eTest/resources")
    compileClasspath += sourceSets.main.get().output
    runtimeClasspath += output + compileClasspath
}

configurations[bindingE2eTest.implementationConfigurationName]
    .extendsFrom(configurations.testImplementation.get())
configurations[bindingE2eTest.runtimeOnlyConfigurationName]
    .extendsFrom(configurations.testRuntimeOnly.get())

tasks.test {
    useJUnitPlatform()
}

tasks.register<Test>("bindingE2eTest") {
    group = "verification"
    description = "Runs the FFmpeg Binding against a real TaskCage daemon and Runtime Package."
    testClassesDirs = bindingE2eTest.output.classesDirs
    classpath = bindingE2eTest.runtimeClasspath
    useJUnitPlatform()
    doFirst {
        listOf("TASKCAGE_SOCKET", "TASKCAGE_ARTIFACT_ROOT").forEach { name ->
            require(!System.getenv(name).isNullOrBlank()) {
                "bindingE2eTest requires $name"
            }
        }
    }
}
