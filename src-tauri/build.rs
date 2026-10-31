fn main() {
    // On Android the audio output (cpal → Oboe) is C++. Newer NDKs keep part of the
    // C++ runtime in libc++abi, which nothing else links, so pull it in here.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-lib=c++abi");
    }
    tauri_build::build()
}
