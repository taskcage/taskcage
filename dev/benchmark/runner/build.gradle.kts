plugins {
    application
}

repositories {
    mavenCentral()
}

dependencies {
    implementation(project(":java-sdk"))
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
