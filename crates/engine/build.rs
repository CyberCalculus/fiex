// Build script for the engine crate — compiles the tiny FICLONE shim.
//
// We can't declare a variadic C function for `ioctl` in Rust FFI safely,
// so we wrap the single call we need in a regular extern "C" function.

fn main() {
    #[cfg(target_os = "linux")]
    {
        cc::Build::new()
            .file("src/ficlone_shim.c")
            .compile("fiex_ficlone_shim");
    }
    println!("cargo:rerun-if-changed=src/ficlone_shim.c");
}
