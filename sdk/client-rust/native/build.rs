// The shared library names itself, so what links it (the JNI bridge, the
// Dart shim) looks for it by name next to them, not by a build path.
fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "linux" || os == "android" {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libagentim_client.so");
    }
}
