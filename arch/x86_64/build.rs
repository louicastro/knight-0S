use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=ap_boot.asm");
    println!("cargo:rerun-if-changed=linker.ld");

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if target_arch != "x86_64" {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR set"));
    let out_bin = out_dir.join("ap_boot.bin");

    let nasm = find_nasm();
    let status = Command::new(&nasm)
        .args(["-f", "bin", "ap_boot.asm", "-o"])
        .arg(&out_bin)
        .status()
        .unwrap_or_else(|e| panic!("failed to invoke NASM ({nasm}): {e}"));

    assert!(status.success(), "NASM failed to assemble ap_boot.asm");
}

fn find_nasm() -> String {
    let win_default = Path::new(r"C:\Program Files\NASM\nasm.exe");
    if win_default.exists() {
        return win_default.to_string_lossy().into_owned();
    }
    "nasm".to_string()
}
