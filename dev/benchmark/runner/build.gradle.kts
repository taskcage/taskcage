plugins {
    application
}

repositories {
    mavenCentral()
}

dependencies {
    implementation(project(":java-sdk"))
    testImplementation(platform("org.junit:junit-bom:5.11.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
}

application {
    mainClass = "org.taskcage.benchmark.ExecutionWorkerBenchmark"
}

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(17)
    }
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 17
}

tasks.test {
    useJUnitPlatform()
}
