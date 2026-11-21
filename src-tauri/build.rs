fn main() {
    println!("cargo:rerun-if-changed=native/tap.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/tap.m")
            .flag("-fobjc-arc")
            .flag("-mmacosx-version-min=14.2")
            .compile("voxa_core_audio");
        for framework in ["CoreAudio", "Foundation", "AppKit", "AVFoundation"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
    tauri_build::build();
}
