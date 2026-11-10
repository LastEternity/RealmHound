fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=wpcap.def");

    let target = std::env::var("TARGET").expect("Cargo must set TARGET");
    if target.ends_with("-windows-msvc") {
        #[cfg(windows)]
        generate_wpcap_import_library(&target);
        #[cfg(not(windows))]
        generate_wpcap_import_library_cross(&target);
    } else if target.ends_with("-windows-gnu") {
        generate_wpcap_import_library_cross(&target);
    }
}

fn generate_wpcap_import_library_cross(_target: &str) {
    use std::path::PathBuf;
    use std::process::Command;

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let wpcap_lib = out_dir.join("wpcap.lib");
    let libwpcap_a = out_dir.join("libwpcap.a");

    // Try `x86_64-w64-mingw32-dlltool` first (from `mingw-w64` on Ubuntu), then `llvm-dlltool`.
    let mingw_ok = Command::new("x86_64-w64-mingw32-dlltool")
        .args(["-d", "wpcap.def", "-l"])
        .arg(&libwpcap_a)
        .status()
        .is_ok_and(|s| s.success());

    if mingw_ok {
        let _ = std::fs::copy(&libwpcap_a, &wpcap_lib);
        println!("cargo:rustc-link-search=native={}", out_dir.display());
        return;
    }

    let llvm_ok = Command::new("llvm-dlltool")
        .args(["-m", "i386:x86-64", "-d", "wpcap.def", "-l"])
        .arg(&wpcap_lib)
        .status()
        .is_ok_and(|s| s.success());

    if llvm_ok {
        let _ = std::fs::copy(&wpcap_lib, &libwpcap_a);
        println!("cargo:rustc-link-search=native={}", out_dir.display());
        return;
    }

    panic!(
        "Cross-compiling for Windows requires either `x86_64-w64-mingw32-dlltool` \
         (sudo apt install mingw-w64) or `llvm-dlltool` (sudo apt install llvm) \
         to generate wpcap.lib from wpcap.def."
    );
}

#[cfg(windows)]
fn generate_wpcap_import_library(target: &str) {
    use std::path::PathBuf;

    let machine = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "X64",
        Ok("aarch64") => "ARM64",
        Ok("x86") => "X86",
        arch => panic!("Unsupported Windows architecture for wpcap: {arch:?}"),
    };
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let mut librarian = find_msvc_tools::find(target, "lib.exe").expect(
        "lib.exe was not found; install Visual Studio Build Tools with \
         Desktop development with C++ and a Windows SDK",
    );
    let output = librarian
        .arg("/nologo")
        .arg(format!("/machine:{machine}"))
        .arg("/def:wpcap.def")
        .arg(format!("/out:{}", out_dir.join("wpcap.lib").display()))
        .output()
        .expect("Failed to run lib.exe to generate the wpcap import library");

    assert!(
        output.status.success(),
        "lib.exe failed to generate the wpcap import library ({}):\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("cargo:rustc-link-search=native={}", out_dir.display());
}
