use std::{env, path::PathBuf, process::Command};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest_dir.join("ui-swift/LauncherBridge.swift");
    println!("cargo:rerun-if-changed={}", source.display());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let archive = out_dir.join("libdesktopctl_launcher_ui.a");
    let sdk = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .expect("xcrun is required to build the macOS launcher UI");
    assert!(sdk.status.success(), "xcrun failed to locate macOS SDK");
    let sdk = String::from_utf8(sdk.stdout)
        .expect("xcrun returned non-UTF-8 SDK path")
        .trim()
        .to_owned();

    let target = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64-apple-macosx12.0",
        Ok("x86_64") => "x86_64-apple-macosx12.0",
        other => panic!("unsupported macOS target architecture: {other:?}"),
    };

    let status = Command::new("swiftc")
        .args([
            "-parse-as-library",
            "-static",
            "-emit-library",
            "-module-name",
            "DesktopCtlLauncherUI",
            "-sdk",
            &sdk,
            "-target",
            target,
            "-framework",
            "AppKit",
            "-framework",
            "Foundation",
            "-framework",
            "SwiftUI",
            "-o",
        ])
        .arg(&archive)
        .arg(&source)
        .status()
        .expect("swiftc is required to build the macOS launcher UI");
    assert!(
        status.success(),
        "swiftc failed to build LauncherBridge.swift"
    );

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    let swiftc = Command::new("xcrun")
        .args(["--find", "swiftc"])
        .output()
        .expect("xcrun is required to locate Swift runtime libraries");
    assert!(swiftc.status.success(), "xcrun failed to locate swiftc");
    let swiftc = PathBuf::from(
        String::from_utf8(swiftc.stdout)
            .expect("xcrun returned non-UTF-8 swiftc path")
            .trim(),
    );
    let swift_runtime = swiftc
        .parent()
        .and_then(|path| path.parent())
        .expect("swiftc has no toolchain directory")
        .join("lib/swift/macosx");
    println!("cargo:rustc-link-search=native={}", swift_runtime.display());
    println!("cargo:rustc-link-lib=static=swiftCompatibility56");
    println!("cargo:rustc-link-lib=static=swiftCompatibilityPacks");
    println!("cargo:rustc-link-lib=static=desktopctl_launcher_ui");
}
