import java.security.KeyStore

plugins {
    id("com.android.application") version "8.10.1"
}

// Rust 工作区根目录，版本号取自那里的 Cargo.toml。
val cargoRoot: File = rootDir.parentFile
val cargoVersion: String = Regex("""(?m)^version\s*=\s*"([^"]+)"""")
    .find(cargoRoot.resolve("Cargo.toml").readText())!!.groupValues[1]
val releaseKeystore: String? = System.getenv("CARGO_APK_RELEASE_KEYSTORE")
val releasePassword: String? = System.getenv("CARGO_APK_RELEASE_KEYSTORE_PASSWORD")

android {
    namespace = "io.github.him049.opensplatcapture"
    compileSdk = 34
    ndkVersion = "27.0.12077973"

    defaultConfig {
        applicationId = "io.github.him049.opensplatcapture"
        minSdk = 29
        targetSdk = 34
        versionName = cargoVersion
        versionCode = cargoVersion.split(".").map(String::toInt)
            .let { (major, minor, patch) -> major * 1_000_000 + minor * 1_000 + patch }
        ndk { abiFilters += "arm64-v8a" }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    signingConfigs {
        // 正式包的密钥库和密码来自环境变量，密钥别名取库里的第一个。
        if (releaseKeystore != null && releasePassword != null) {
            create("release") {
                storeFile = file(releaseKeystore)
                storePassword = releasePassword
                keyPassword = releasePassword
                keyAlias = KeyStore.getInstance(storeFile, releasePassword.toCharArray()).aliases().nextElement()
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.findByName("release")
        }
    }

    sourceSets {
        getByName("debug").jniLibs.srcDir(layout.buildDirectory.dir("rust/debug"))
        getByName("release").jniLibs.srcDir(layout.buildDirectory.dir("rust/release"))
    }
}

dependencies {
    implementation("org.khronos.openxr:openxr_loader_for_android:1.1.65")
}

// 用和 Gradle 相同的 NDK 编译 Rust 库，产物放进对应构建类型的 jniLibs。
for (profile in listOf("debug", "release")) {
    val cargoBuild = tasks.register<Exec>("cargoBuild${profile.replaceFirstChar(Char::uppercase)}") {
        workingDir = cargoRoot
        val output = layout.buildDirectory.dir("rust/$profile").get().asFile
        commandLine(
            listOf("cargo", "ndk", "-t", "arm64-v8a", "-P", "29", "-o", output.path, "build", "-p", "open_capture", "--lib") +
                if (profile == "release") listOf("--release") else emptyList()
        )
        doFirst { environment("ANDROID_NDK_HOME", android.ndkDirectory.path) }
    }
    tasks.configureEach {
        if (name == "merge${profile.replaceFirstChar(Char::uppercase)}JniLibFolders") dependsOn(cargoBuild)
    }
}
