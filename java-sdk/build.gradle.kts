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

configurations[e2eTest.implementationConfigurationName].extendsFrom(configurations.testImplementation.get())
configurations[e2eTest.runtimeOnlyConfigurationName].extendsFrom(configurations.testRuntimeOnly.get())

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
    }
}
