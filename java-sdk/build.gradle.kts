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
    api("com.fasterxml.jackson.core:jackson-databind:2.19.1")
    testImplementation(platform("org.junit:junit-bom:5.11.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
}

tasks.test {
    useJUnitPlatform()
}
