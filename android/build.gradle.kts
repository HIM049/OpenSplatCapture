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
// 每个变体对应的 Rust 包。
val rustPackages = mapOf("capture" to "open_capture", "probe" to "open_capture_probe")
val profiles = listOf("debug", "release")
fun String.cap() = replaceFirstChar(Char::uppercase)

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

    // capture 是拍摄程序本身，probe 是检查设备能力的探测程序，两者可以同时安装。
    flavorDimensions += "app"
    productFlavors {
        create("capture") {
            dimension = "app"
            isDefault = true
        }
        create("probe") {
            dimension = "app"
            applicationIdSuffix = ".probe"
            versionNameSuffix = "-probe"
        }
    }

    sourceSets {
        for ((flavor, _) in rustPackages) {
            for (profile in profiles) {
                maybeCreate("$flavor${profile.cap()}").jniLibs.srcDir(layout.buildDirectory.dir("rust/$flavor/$profile"))
            }
        }
    }
}

dependencies {
    implementation("org.khronos.openxr:openxr_loader_for_android:1.1.65")
}

// 用和 Gradle 相同的 NDK 编译各变体的 Rust 库，产物放进对应变体的 jniLibs。
for ((flavor, cargoPackage) in rustPackages) {
    for (profile in profiles) {
        val variant = "${flavor.cap()}${profile.cap()}"
        val cargoBuild = tasks.register<Exec>("cargoBuild$variant") {
            workingDir = cargoRoot
            val output = layout.buildDirectory.dir("rust/$flavor/$profile").get().asFile
            commandLine(
                listOf("cargo", "ndk", "-t", "arm64-v8a", "-P", "29", "-o", output.path, "build", "-p", cargoPackage, "--lib") +
                    if (profile == "release") listOf("--release") else emptyList()
            )
            doFirst { environment("ANDROID_NDK_HOME", android.ndkDirectory.path) }
            // 每次都交给 cargo 判断是否需要重新编译。
            outputs.upToDateWhen { false }
        }
        // AGP 合并 jniLibs 时察觉不到 Rust 库的变化，所以每次都重新合并。
        tasks.configureEach {
            if (name == "merge${variant}JniLibFolders") {
                dependsOn(cargoBuild)
                outputs.upToDateWhen { false }
            }
        }
    }
}
