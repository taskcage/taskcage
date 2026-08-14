plugins {
    java
}

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(17)
    }
}

repositories {
    exclusiveContent {
        forRepository {
            maven {
                name = "TaskCageReleaseCandidate"
                url = uri(requireNotNull(System.getenv("TASKCAGE_RELEASE_REPOSITORY")))
            }
        }
        filter {
            includeGroup("org.taskcage")
        }
    }
    mavenCentral()
}

dependencies {
    implementation("org.taskcage:taskcage-java-sdk:${requireNotNull(System.getenv("TASKCAGE_JAVA_SDK_VERSION"))}")
}
