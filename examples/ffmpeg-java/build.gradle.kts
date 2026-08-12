plugins {
    application
}

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(17)
    }
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("org.taskcage:taskcage-java-sdk:0.2.0")
    implementation("org.taskcage:taskcage-ffmpeg-binding:0.1.0")
}

application {
    mainClass = "org.taskcage.example.ffmpeg.FfmpegExample"
}
