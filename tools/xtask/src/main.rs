//! KnightOS build automation, KPKG initrd packager, and QEMU test runner (`cargo xtask`).

use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use knight_abi::{KPKG_MAGIC, KpkgEntry, KpkgHeader};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arch {
    X86_64,
    AArch64,
}

impl Arch {
    fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::AArch64 => "aarch64",
        }
    }

    fn uefi_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-uefi",
            Arch::AArch64 => "aarch64-unknown-uefi",
        }
    }

    fn bare_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-none",
            Arch::AArch64 => "aarch64-unknown-none",
        }
    }

    fn boot_efi_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "BOOTX64.EFI",
            Arch::AArch64 => "BOOTAA64.EFI",
        }
    }

    fn esp_dir_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "esp-x86_64",
            Arch::AArch64 => "esp-aarch64",
        }
    }

    fn default_kernel_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "kernel-x86_64.elf",
            Arch::AArch64 => "kernel-aarch64.elf",
        }
    }

    fn default_initrd_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "initrd-x86_64.kpkg",
            Arch::AArch64 => "initrd-aarch64.kpkg",
        }
    }

    fn qemu_bin(self) -> &'static str {
        match self {
            Arch::X86_64 => "qemu-system-x86_64",
            Arch::AArch64 => "qemu-system-aarch64",
        }
    }

    fn edk2_firmware_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "edk2-x86_64-code.fd",
            Arch::AArch64 => "edk2-aarch64-code.fd",
        }
    }

    fn kernel_rustflags(self) -> &'static str {
        match self {
            Arch::X86_64 => "-C link-arg=-Tarch/x86_64/linker.ld -C relocation-model=static -C code-model=kernel",
            Arch::AArch64 => "-C link-arg=-Tarch/aarch64/linker.ld -C relocation-model=static",
        }
    }

    fn user_rustflags(self) -> &'static str {
        match self {
            Arch::X86_64 | Arch::AArch64 => "-C link-arg=-Tservices/initd/user.ld -C relocation-model=static",
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("tools/xtask lives two levels below workspace root")
        .to_path_buf()
}

fn parse_arches(filter: Option<&str>) -> Result<Vec<Arch>, String> {
    match filter.unwrap_or("all") {
        "x86_64" => Ok(vec![Arch::X86_64]),
        "aarch64" | "arm64" => Ok(vec![Arch::AArch64]),
        "all" => Ok(vec![Arch::X86_64, Arch::AArch64]),
        other => Err(format!(
            "unsupported --arch `{other}` (expected x86_64, aarch64, or all)"
        )),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");

    let release = args.iter().any(|a| a == "--release");
    let display = args.iter().any(|a| a == "--gui");
    let arch_filter = args
        .windows(2)
        .find(|w| w[0] == "--arch")
        .map(|w| w[1].as_str());

    let res = match cmd {
        "build-boot" => build_boot(arch_filter, release),
        "build-kernel" => build_kernel(arch_filter, release),
        "build-initrd" => build_initrd(arch_filter, release),
        "build" | "build-all" => build_boot(arch_filter, release)
            .and_then(|_| build_kernel(arch_filter, release))
            .and_then(|_| build_initrd(arch_filter, release)),
        "run" => run_qemu(arch_filter, release, display),
        "qemu-test" => qemu_smoke_test(arch_filter, release),
        "test" => run_host_tests(),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => Err(format!("unknown subcommand `{other}`")),
    };

    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xtask error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    println!(
        "\
KnightOS Build Automation (xtask)
USAGE:
  cargo xtask build-boot [--arch x86_64|aarch64|all] [--release]
  cargo xtask build-kernel [--arch x86_64|aarch64|all] [--release]
  cargo xtask build-initrd [--arch x86_64|aarch64|all] [--release]
  cargo xtask build [--arch x86_64|aarch64|all] [--release]
  cargo xtask run [--arch x86_64|aarch64] [--release] [--gui]
  cargo xtask qemu-test [--arch x86_64|aarch64|all] [--release]
  cargo xtask test
"
    );
}

fn stage_esp_config(esp_os_dir: &Path, arch: Arch) -> Result<(), String> {
    let cfg_path = esp_os_dir.join("knightboot.toml");
    let default_cfg = format!(
        "# Default KnightOS bootloader configuration\nslot = \"A\"\nkernel = \"\\\\EFI\\\\KNIGHTOS\\\\{}\"\ninitrd = \"\\\\EFI\\\\KNIGHTOS\\\\{}\"\ncmdline = \"console=serial\"\nkaslr = true\n",
        arch.default_kernel_name(),
        arch.default_initrd_name()
    );
    fs::write(&cfg_path, default_cfg).map_err(|e| e.to_string())
}

fn build_boot(arch_filter: Option<&str>, release: bool) -> Result<(), String> {
    let arches = parse_arches(arch_filter)?;
    let root = workspace_root();
    let profile = if release { "release" } else { "debug" };

    for arch in arches {
        println!("==> Building knightboot for {}", arch.uefi_target());
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&root)
            .env_remove("RUSTFLAGS")
            .args(["build", "-p", "knightboot", "--target", arch.uefi_target()]);
        if release {
            cmd.arg("--release");
        }
        let status = cmd.status().map_err(|e| format!("failed to spawn cargo: {e}"))?;
        if !status.success() {
            return Err(format!("cargo build failed for {}", arch.uefi_target()));
        }

        let built_efi = root
            .join("target")
            .join(arch.uefi_target())
            .join(profile)
            .join("knightboot.efi");
        if !built_efi.exists() {
            return Err(format!("expected EFI artifact missing: {}", built_efi.display()));
        }

        let esp_boot_dir = root.join("build").join(arch.esp_dir_name()).join("EFI").join("BOOT");
        let esp_os_dir = root.join("build").join(arch.esp_dir_name()).join("EFI").join("KNIGHTOS");
        fs::create_dir_all(&esp_boot_dir).map_err(|e| e.to_string())?;
        fs::create_dir_all(&esp_os_dir).map_err(|e| e.to_string())?;

        let dest_efi = esp_boot_dir.join(arch.boot_efi_name());
        fs::copy(&built_efi, &dest_efi)
            .map_err(|e| format!("failed to copy {}: {e}", dest_efi.display()))?;
        stage_esp_config(&esp_os_dir, arch)?;
        println!("    Staged {}", dest_efi.display());
    }
    Ok(())
}

fn build_kernel(arch_filter: Option<&str>, release: bool) -> Result<(), String> {
    let arches = parse_arches(arch_filter)?;
    let root = workspace_root();
    let profile = if release { "release" } else { "debug" };

    for arch in arches {
        println!("==> Building knightos-kernel for {}", arch.bare_target());
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&root)
            .env("RUSTFLAGS", arch.kernel_rustflags())
            .args(["build", "-p", "knightos-kernel", "--target", arch.bare_target()]);
        if release {
            cmd.arg("--release");
        }
        let status = cmd.status().map_err(|e| format!("failed to spawn cargo: {e}"))?;
        if !status.success() {
            return Err(format!("kernel build failed for {}", arch.bare_target()));
        }

        let built_elf = root
            .join("target")
            .join(arch.bare_target())
            .join(profile)
            .join("knightos-kernel");
        if !built_elf.exists() {
            return Err(format!("expected kernel ELF missing: {}", built_elf.display()));
        }

        let esp_os_dir = root.join("build").join(arch.esp_dir_name()).join("EFI").join("KNIGHTOS");
        fs::create_dir_all(&esp_os_dir).map_err(|e| e.to_string())?;
        let dest_elf = esp_os_dir.join(arch.default_kernel_name());
        fs::copy(&built_elf, &dest_elf)
            .map_err(|e| format!("failed to copy {}: {e}", dest_elf.display()))?;
        stage_esp_config(&esp_os_dir, arch)?;
        println!("    Staged {}", dest_elf.display());
    }
    Ok(())
}

fn build_initrd(arch_filter: Option<&str>, release: bool) -> Result<(), String> {
    let arches = parse_arches(arch_filter)?;
    let root = workspace_root();
    let profile = if release { "release" } else { "debug" };

    for arch in arches {
        for (pkg, bin) in [("initd", "initd"), ("knight-shell", "shell")] {
            println!("==> Building {bin} user service for {}", arch.bare_target());
            let mut cmd = Command::new("cargo");
            cmd.current_dir(&root)
                .env("RUSTFLAGS", arch.user_rustflags())
                .args(["build", "-p", pkg, "--bin", bin, "--target", arch.bare_target()]);
            if release {
                cmd.arg("--release");
            }
            let status = cmd.status().map_err(|e| format!("failed to spawn cargo: {e}"))?;
            if !status.success() {
                return Err(format!("{bin} build failed for {}", arch.bare_target()));
            }
        }

        let initd_bytes = fs::read(
            root.join("target")
                .join(arch.bare_target())
                .join(profile)
                .join("initd"),
        )
        .map_err(|e| format!("failed to read initd ELF: {e}"))?;
        let shell_bytes = fs::read(
            root.join("target")
                .join(arch.bare_target())
                .join(profile)
                .join("shell"),
        )
        .map_err(|e| format!("failed to read shell ELF: {e}"))?;

        let kpkg_bytes = pack_kpkg(&[("initd.elf", &initd_bytes), ("shell.elf", &shell_bytes)]);
        let esp_os_dir = root.join("build").join(arch.esp_dir_name()).join("EFI").join("KNIGHTOS");
        fs::create_dir_all(&esp_os_dir).map_err(|e| e.to_string())?;
        let dest_kpkg = esp_os_dir.join(arch.default_initrd_name());
        fs::write(&dest_kpkg, &kpkg_bytes)
            .map_err(|e| format!("failed to write {}: {e}", dest_kpkg.display()))?;
        println!(
            "    Packaged {} ({} bytes, initd={}B, shell={}B)",
            dest_kpkg.display(),
            kpkg_bytes.len(),
            initd_bytes.len(),
            shell_bytes.len()
        );
    }
    Ok(())
}

fn pack_kpkg(files: &[(&str, &[u8])]) -> Vec<u8> {
    let header_size = size_of::<KpkgHeader>();
    let table_size = files.len() * size_of::<KpkgEntry>();
    let data_start = align_up(header_size + table_size, 16);

    let mut out = vec![0u8; data_start];
    let hdr = KpkgHeader {
        magic: KPKG_MAGIC,
        entry_count: files.len() as u32,
        _reserved: 0,
    };
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&hdr as *const KpkgHeader) as *const u8,
            out.as_mut_ptr(),
            header_size,
        );
    }

    for (idx, &(name, data)) in files.iter().enumerate() {
        let off = out.len();
        out.extend_from_slice(data);
        let padded_len = align_up(out.len(), 16);
        out.resize(padded_len, 0);

        let mut ent = KpkgEntry {
            name: [0; 64],
            offset: off as u64,
            size: data.len() as u64,
        };
        let nb = name.as_bytes();
        let copy_len = nb.len().min(63);
        ent.name[..copy_len].copy_from_slice(&nb[..copy_len]);

        let ent_pos = header_size + idx * size_of::<KpkgEntry>();
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&ent as *const KpkgEntry) as *const u8,
                out[ent_pos..].as_mut_ptr(),
                size_of::<KpkgEntry>(),
            );
        }
    }

    out
}

fn align_up(v: usize, align: usize) -> usize {
    (v + align - 1) & !(align - 1)
}

fn run_host_tests() -> Result<(), String> {
    let root = workspace_root();
    let status = Command::new("cargo")
        .current_dir(&root)
        .env_remove("RUSTFLAGS")
        .args([
            "test",
            "-p",
            "knightboot-protocol",
            "-p",
            "knightboot",
            "-p",
            "knight-abi",
            "-p",
            "knightos-hal",
            "-p",
            "knightos-cap",
            "-p",
            "knightos-ipc",
            "-p",
            "knightos-sched",
            "-p",
            "knightos-mm",
            "-p",
            "knightos-input",
            "-p",
            "knightos-nvme",
            "-p",
            "knightos-virtio",
            "-p",
            "knightos-devmgr",
            "-p",
            "knightfs",
            "-p",
            "knightos-vfs",
            "-p",
            "knightos-gpu-hal",
            "-p",
            "knightos-themes",
            "-p",
            "knight-compositor",
            "-p",
            "knight-ui-kit",
            "-p",
            "knight-terminal",
            "-p",
            "knight-settings",
            "-p",
            "knight-shell",
        ])
        .status()
        .map_err(|e| format!("failed to spawn cargo test: {e}"))?;
    if !status.success() {
        return Err("host unit tests failed".to_string());
    }
    Ok(())
}

fn find_qemu_and_firmware(arch: Arch) -> Result<(PathBuf, PathBuf), String> {
    let exe = if cfg!(windows) {
        format!("{}.exe", arch.qemu_bin())
    } else {
        arch.qemu_bin().to_string()
    };

    let qemu_candidates = [
        PathBuf::from(&exe),
        PathBuf::from(r"C:\Program Files\qemu").join(&exe),
    ];
    let qemu_path = qemu_candidates
        .into_iter()
        .find(|p| p.exists() || which_exists(p))
        .ok_or_else(|| format!("{} not found on PATH or C:\\Program Files\\qemu", arch.qemu_bin()))?;

    let fw_name = arch.edk2_firmware_name();
    let fw_candidates = [
        PathBuf::from(r"C:\Program Files\qemu\share").join(fw_name),
        PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd"),
        PathBuf::from("/usr/share/qemu").join(fw_name),
        PathBuf::from("/usr/share/AAVMF/AAVMF_CODE.fd"),
    ];
    let fw_path = fw_candidates
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| format!("UEFI firmware {fw_name} not found"))?;

    Ok((qemu_path, fw_path))
}

fn which_exists(p: &Path) -> bool {
    if p.components().count() > 1 {
        return p.exists();
    }
    let Some(path_var) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path_var).any(|dir| dir.join(p).exists())
}

fn ensure_raw_image(root: &Path, name: &str, bytes: u64) -> Result<PathBuf, String> {
    let disk_path = root.join("build").join(name);
    if !disk_path.exists() {
        if let Some(parent) = disk_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = fs::File::create(&disk_path)
            .map_err(|e| format!("failed to create {}: {e}", disk_path.display()))?;
        file.set_len(bytes)
            .map_err(|e| format!("failed to size {}: {e}", disk_path.display()))?;
    }
    Ok(disk_path)
}

fn qemu_command(arch: Arch, root: &Path, gui: bool) -> Result<Command, String> {
    let (qemu, fw) = find_qemu_and_firmware(arch)?;
    let esp_dir = root.join("build").join(arch.esp_dir_name());
    let disk_path = ensure_raw_image(root, &format!("disk-{}.img", arch.name()), 16 * 1024 * 1024)?;
    let nvme_path = ensure_raw_image(root, &format!("nvme-{}.img", arch.name()), 8 * 1024 * 1024)?;
    let fat_arg = format!("format=raw,file=fat:rw:{}", esp_dir.display());
    let pflash_arg = format!("if=pflash,format=raw,readonly=on,file={}", fw.display());
    let blk_drive_arg = format!("if=none,format=raw,id=hd0,file={}", disk_path.display());
    let nvme_drive_arg = format!("if=none,format=raw,id=nv0,file={}", nvme_path.display());

    let mut cmd = Command::new(qemu);
    match arch {
        Arch::X86_64 => {
            cmd.args([
                "-machine", "q35,accel=tcg",
                "-cpu", "qemu64,+ssse3,+sse4.1,+sse4.2,+x2apic",
                "-smp", "4",
                "-m", "1024M",
                "-drive", &pflash_arg,
                "-drive", &fat_arg,
                "-drive", &blk_drive_arg,
                "-device", "virtio-blk-pci,drive=hd0",
                "-drive", &nvme_drive_arg,
                "-device", "nvme,serial=knight0,drive=nv0",
                "-serial", "stdio",
                "-no-reboot",
                "-no-shutdown",
            ]);
            if !gui {
                cmd.args(["-display", "none"]);
            }
        }
        Arch::AArch64 => {
            cmd.args([
                "-machine", "virt,gic-version=3",
                "-cpu", "cortex-a72",
                "-smp", "4",
                "-m", "1024M",
                "-drive", &pflash_arg,
                "-drive", &fat_arg,
                "-drive", &blk_drive_arg,
                "-device", "virtio-blk-pci,drive=hd0",
                "-drive", &nvme_drive_arg,
                "-device", "nvme,serial=knight0,drive=nv0",
                "-device", "virtio-gpu-pci",
                "-serial", "stdio",
                "-no-reboot",
                "-no-shutdown",
            ]);
            if !gui {
                cmd.args(["-display", "none"]);
            }
        }
    }
    Ok(cmd)
}

fn run_qemu(arch_filter: Option<&str>, release: bool, gui: bool) -> Result<(), String> {
    let arches = parse_arches(arch_filter)?;
    let arch = arches[0];
    build_boot(Some(arch.name()), release)?;
    build_kernel(Some(arch.name()), release)?;
    build_initrd(Some(arch.name()), release)?;

    let root = workspace_root();
    let mut cmd = qemu_command(arch, &root, gui)?;
    println!("==> Launching QEMU ({})...", arch.name());
    let status = cmd.status().map_err(|e| format!("failed to launch QEMU: {e}"))?;
    if !status.success() {
        return Err(format!("QEMU exited with status {status}"));
    }
    Ok(())
}

fn qemu_smoke_test(arch_filter: Option<&str>, release: bool) -> Result<(), String> {
    let arches = parse_arches(arch_filter)?;
    build_boot(arch_filter, release)?;
    build_kernel(arch_filter, release)?;
    build_initrd(arch_filter, release)?;

    let root = workspace_root();
    for arch in arches {
        println!("==> Running headless QEMU UEFI smoke test ({}, -smp 4)...", arch.name());
        let mut cmd = qemu_command(arch, &root, false)?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn QEMU: {e}"))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = mpsc::channel::<String>();

        let reader_thread = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });

        let timeout = Duration::from_secs(45);
        let start = Instant::now();
        let mut phase2_ok = false;
        let mut phase3_ok = false;
        let mut phase4_ok = false;

        while start.elapsed() < timeout {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    println!("    [qemu:{}] {}", arch.name(), line);
                    if line.contains("[KNIGHTOS PHASE2 PASS]") && line.contains("cpus=4/4") {
                        phase2_ok = true;
                    }
                    if line.contains("[KNIGHTOS PHASE3 PASS]")
                        && line.contains("smp_sched=ok")
                        && line.contains("virtio_blk=ok")
                        && line.contains("knightfs=ok")
                    {
                        phase3_ok = true;
                    }
                    if line.contains("[KNIGHTOS PHASE4 PASS]")
                        && line.contains("spawn=ok")
                        && line.contains("compositor=ok")
                        && line.contains("shell=ok")
                        && line.contains("nvme=ok")
                        && line.contains("knightfs2=ok")
                    {
                        phase4_ok = true;
                    }
                    if phase2_ok && phase3_ok && phase4_ok {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        return Err(format!(
                            "QEMU ({}) exited prematurely with {status}",
                            arch.name()
                        ));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        let _ = reader_thread.join();

        if !(phase2_ok && phase3_ok && phase4_ok) {
            return Err(format!(
                "QEMU ({}) smoke test incomplete (phase2_ok={}, phase3_ok={}, phase4_ok={})",
                arch.name(),
                phase2_ok,
                phase3_ok,
                phase4_ok
            ));
        }
        println!("==> QEMU ({}) Phase 2 + Phase 3 + Phase 4 smoke test PASSED!", arch.name());
    }

    Ok(())
}
