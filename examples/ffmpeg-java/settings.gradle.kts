pluginManagement {
    repositories {
        gradlePluginPortal()
    }
}

rootProject.name = "taskcage-ffmpeg-java-example"

includeBuild("../../java-sdk")
includeBuild("../../java-bindings/ffmpeg")
