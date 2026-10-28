// Compiles Signalsmith Stretch (header-only C++) behind src/wrapper.cpp. No
// bindgen: the few functions are declared by hand in src/lib.rs, so building
// needs a C++ compiler and nothing else. Always optimised, even in debug
// builds, so the player can shift a key live.
fn main() {
    println!("cargo:rerun-if-changed=src/wrapper.cpp");
    println!("cargo:rerun-if-changed=vendor");
    let mut b = cc::Build::new();
    b.cpp(true)
        .file("src/wrapper.cpp")
        .include("vendor/signalsmith-stretch")
        .include("vendor")
        .opt_level(3)
        .debug(false)
        .warnings(false);
    let cc = b.get_compiler();
    if cc.is_like_msvc() {
        b.flag("/std:c++14").flag("/EHsc");
        // Cross-building for Windows (cargo-xwin) uses the distribution's
        // clang-cl with the newest MSVC headers, which ask for a newer clang
        // than Ubuntu ships; the few parts of the STL used here don't mind.
        if cc.is_like_clang_cl() {
            b.define("_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH", None);
        }
    } else {
        b.flag("-std=c++14");
    }
    b.compile("fatstick_stretch");
}
