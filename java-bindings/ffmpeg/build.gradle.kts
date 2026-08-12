import org.gradle.api.publish.maven.MavenPublication

plugins {
    `java-library`
    `maven-publish`
    signing
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

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            from(components["java"])
            artifactId = "taskcage-ffmpeg-binding"
            pom {
                name = "TaskCage FFmpeg Binding"
                description = "Typed Java FFmpeg operations for the TaskCage process runtime"
                url = "https://github.com/taskcage/taskcage"
                licenses {
                    license {
                        name = "The Apache License, Version 2.0"
                        url = "https://www.apache.org/licenses/LICENSE-2.0.txt"
                        distribution = "repo"
                    }
                }
                developers {
                    developer {
                        id = "taskcage"
                        name = "TaskCage contributors"
                        url = "https://github.com/taskcage"
                    }
                }
                scm {
                    connection = "scm:git:https://github.com/taskcage/taskcage.git"
                    developerConnection = "scm:git:ssh://git@github.com/taskcage/taskcage.git"
                    url = "https://github.com/taskcage/taskcage"
                }
            }
        }
    }
    repositories {
        maven {
            name = "CentralBundle"
            url = uri(layout.buildDirectory.dir("central-repository"))
        }
    }
}

val mavenSigningKey = providers.environmentVariable("MAVEN_SIGNING_KEY")
val mavenSigningPassword = providers.environmentVariable("MAVEN_SIGNING_PASSWORD")

signing {
    if (mavenSigningKey.isPresent) {
        useInMemoryPgpKeys(mavenSigningKey.get(), mavenSigningPassword.orNull)
    }
    sign(publishing.publications["mavenJava"])
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
