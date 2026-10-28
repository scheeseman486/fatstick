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
    if b.get_compiler().is_like_msvc() {
        b.flag("/std:c++14").flag("/EHsc");
    } else {
        b.flag("-std=c++14");
    }
    b.compile("p2u_stretch");
}
