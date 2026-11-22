//! Multi-architecture tests.
//!
//! * `port-test`: builds the portable bring-up kernel (`ports/coco-port`)
//!   for RISC-V 64, RISC-V 32, AArch64 and 32-bit x86, boots each in QEMU
//!   and checks exit status and serial output. Each architecture also runs
//!   two negative cases (an injected failing check, an injected illegal
//!   instruction) that must be reported as failures — this proves the pass
//!   verdict is not vacuous.
//! * `cross-test`: runs the portable crates' unit tests on 32-bit x86
//!   natively and on AArch64 / RISC-V 64 under QEMU user-mode emulation, and
//!   compiles them for every bare-metal target.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Res, run};

/// Crates that must build and pass their tests on every architecture.
pub const PORTABLE: &[&str] =
    &["coco-abi", "coco-acpi", "coco-capability", "coco-elf", "coco-fdt", "coco-ipc", "coco-memory", "coco-sync"];

/// One architecture port.
pub struct Port {
    /// Short name used in reports and filters.
    pub name: &'static str,
    /// Rust target triple.
    pub target: &'static str,
    /// Extra rustc flags for this target.
    pub rustflags: &'static [&'static str],
    qemu: &'static str,
    args: &'static [&'static str],
    /// QEMU exit status on success.
    pass_status: i32,
    /// QEMU exit status on failure, if the platform can signal one.
    fail_status: Option<i32>,
}

/// The four ports.
pub const PORTS: &[Port] = &[
    Port {
        name: "riscv64",
        target: "riscv64gc-unknown-none-elf",
        rustflags: &[],
        qemu: "qemu-system-riscv64",
        args: &["-machine", "virt", "-bios", "default"],
        pass_status: 0,
        fail_status: Some(1),
    },
    Port {
        name: "riscv32",
        target: "riscv32imac-unknown-none-elf",
        rustflags: &[],
        qemu: "qemu-system-riscv32",
        args: &["-machine", "virt", "-bios", "none"],
        pass_status: 0,
        fail_status: Some(1),
    },
    Port {
        name: "aarch64",
        target: "aarch64-unknown-none-softfloat",
        rustflags: &[],
        qemu: "qemu-system-aarch64",
        args: &["-machine", "virt,gic-version=2", "-cpu", "cortex-a53"],
        pass_status: 0,
        // PSCI SYSTEM_RESET under -no-reboot also exits with 0: failures are
        // detected from the serial log only.
        fail_status: None,
    },
    Port {
        name: "x86",
        target: "i686-unknown-linux-gnu",
        // The hosted i686 target is used freestanding.
        rustflags: &["-C", "panic=abort", "-C", "relocation-model=static"],
        qemu: "qemu-system-i386",
        args: &["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"],
        pass_status: 33,
        fail_status: Some(35),
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    Normal,
    CheckFailure,
    Fault,
}

impl Variant {
    fn feature(self) -> Option<&'static str> {
        match self {
            Self::Normal => None,
            Self::CheckFailure => Some("inject-check-failure"),
            Self::Fault => Some("inject-fault"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Normal => "self-tests",
            Self::CheckFailure => "negative: failing check",
            Self::Fault => "negative: illegal instruction",
        }
    }
}

/// Encoded rustflags for a port build (target flags + path remapping).
pub fn port_rustflags(root: &Path, port: &Port) -> String {
    let mut flags: Vec<String> = port.rustflags.iter().map(|s| (*s).to_string()).collect();
    flags.extend(crate::artifacts::remap_flags(root));
    flags.join("\u{1f}")
}

fn build(root: &Path, port: &Port, variant: Variant) -> Res<std::path::PathBuf> {
    let target_dir = match variant.feature() {
        None => root.join("target"),
        Some(f) => root.join("target/ports").join(f),
    };
    let mut c = crate::artifacts::cargo(root);
    c.args(["build", "--locked", "--release", "-p", "coco-port", "--target", port.target]);
    c.arg("--target-dir").arg(&target_dir);
    if let Some(f) = variant.feature() {
        c.args(["--features", f]);
    }
    c.env("CARGO_ENCODED_RUSTFLAGS", port_rustflags(root, port));
    run(&mut c)?;
    Ok(target_dir.join(port.target).join("release/coco-port"))
}

fn boot(port: &Port, elf: &Path, log: &Path) -> Res<(Option<i32>, String, f64)> {
    let _ = fs::remove_file(log);
    let mut cmd = Command::new(port.qemu);
    cmd.args(port.args)
        .args(["-smp", "1", "-m", "128M", "-display", "none", "-monitor", "none", "-no-reboot"])
        .arg("-serial")
        .arg(format!("file:{}", log.display()))
        .arg("-kernel")
        .arg(elf)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let start = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("spawn {}: {e}", port.qemu))?;
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| format!("wait: {e}"))? {
            break st.code();
        }
        if start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = String::from_utf8_lossy(&fs::read(log).unwrap_or_default()).replace('\r', "");
    Ok((status, text, start.elapsed().as_secs_f64()))
}

fn verdict(port: &Port, variant: Variant, status: Option<i32>, log: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut need = |cond: bool, what: String| {
        if !cond {
            problems.push(what);
        }
    };
    match variant {
        Variant::Normal => {
            need(status == Some(port.pass_status), format!("exit status {status:?}, expected {}", port.pass_status));
            need(log.contains("coco-port: ALL TESTS PASSED"), "no pass line".into());
            for bad in ["FAIL", "FATAL", "PANIC"] {
                need(!log.contains(bad), format!("{bad:?} in log"));
            }
            need(log.matches(": PASS (").count() == 5, "expected 5 passing checks".into());
        }
        Variant::CheckFailure | Variant::Fault => {
            if let Some(fail) = port.fail_status {
                need(status == Some(fail), format!("exit status {status:?}, expected {fail}"));
            } else {
                need(status.is_some(), "machine did not stop".into());
            }
            need(!log.contains("ALL TESTS PASSED"), "pass line printed by a failing run".into());
            let marker = if variant == Variant::Fault { "FATAL:" } else { "1 TEST(S) FAILED" };
            need(log.contains(marker), format!("missing {marker:?}"));
        }
    }
    problems
}

/// Builds the normal (non-injected) release image of `port` in `root`.
pub fn build_release(root: &Path, port: &Port) -> Res<std::path::PathBuf> {
    build(root, port, Variant::Normal)
}

/// Builds and boots every port; writes `target/coco/port-results.md`.
pub fn port_test(root: &Path, filter: Option<&str>) -> Res<()> {
    let out_dir = root.join("target/coco/ports");
    fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir: {e}"))?;
    let mut table = String::from(
        "# Port boot test results\n\n| Port | Target | Case | Result | ELF size (B) | Wall time (s) | Detail |\n|---|---|---|---|---:|---:|---|\n",
    );
    let (mut passed, mut failed) = (0, 0);
    for port in PORTS.iter().filter(|p| filter.is_none_or(|f| p.name.contains(f))) {
        for variant in [Variant::Normal, Variant::CheckFailure, Variant::Fault] {
            let elf = build(root, port, variant)?;
            let size = fs::metadata(&elf).map_err(|e| format!("{}: {e}", elf.display()))?.len();
            let log = out_dir.join(format!("{}-{}.log", port.name, variant.feature().unwrap_or("normal")));
            let (status, text, secs) = boot(port, &elf, &log)?;
            let problems = verdict(port, variant, status, &text);
            let ok = problems.is_empty();
            let detail = if ok {
                match variant {
                    Variant::Normal => text
                        .lines()
                        .find(|l| l.contains("timer interrupts"))
                        .map_or_else(String::new, |l| l.trim().to_string()),
                    _ => "failure detected as required".into(),
                }
            } else {
                problems.join("; ")
            };
            let v = if ok { "PASS" } else { "FAIL" };
            println!("{v}  {:<8} {:<30} {secs:>6.2}s  {detail}", port.name, variant.label());
            let _ = writeln!(
                table,
                "| {} | `{}` | {} | {v} | {size} | {secs:.2} | {detail} |",
                port.name,
                port.target,
                variant.label()
            );
            if ok { passed += 1 } else { failed += 1 }
        }
    }
    let _ = writeln!(table, "\n**{passed} passed, {failed} failed.** QEMU TCG; logs in `target/coco/ports/`.");
    fs::write(root.join("target/coco/port-results.md"), &table).map_err(|e| format!("write results: {e}"))?;
    println!("\n{passed} passed, {failed} failed");
    if failed == 0 { Ok(()) } else { Err(format!("{failed} port case(s) failed")) }
}

/// Runs the portable crates' tests on 32-bit x86, AArch64 and RISC-V 64,
/// and compiles them for every bare-metal target.
pub fn cross_test(root: &Path) -> Res<()> {
    let mut failed = Vec::new();
    let mut step = |name: &str, c: &mut Command| {
        println!("==> {name}");
        match run(c) {
            Ok(()) => println!("    PASS {name}"),
            Err(e) => {
                println!("    FAIL {name}: {e}");
                failed.push(name.to_string());
            }
        }
    };
    let packages: Vec<&str> = PORTABLE.iter().flat_map(|p| ["-p", p]).collect();

    let mut c = crate::artifacts::cargo(root);
    c.args(["test", "--locked", "--target", "i686-unknown-linux-gnu", "--target-dir", "target/cross"]).args(&packages);
    step("unit tests, i686-unknown-linux-gnu (32-bit, native)", &mut c);

    for (target, runner) in
        [("aarch64-unknown-linux-musl", "qemu-aarch64"), ("riscv64gc-unknown-linux-musl", "qemu-riscv64")]
    {
        let var = target.to_uppercase().replace('-', "_");
        let mut c = crate::artifacts::cargo(root);
        c.args(["test", "--locked", "--target", target, "--target-dir", "target/cross"]).args(&packages);
        c.env(format!("CARGO_TARGET_{var}_LINKER"), "rust-lld")
            .env(format!("CARGO_TARGET_{var}_RUNNER"), runner)
            .env(format!("CARGO_TARGET_{var}_RUSTFLAGS"), "-C target-feature=+crt-static -C link-self-contained=yes");
        step(&format!("unit tests, {target} (QEMU user mode)"), &mut c);
    }

    for target in [
        "riscv64gc-unknown-none-elf",
        "riscv32imac-unknown-none-elf",
        "aarch64-unknown-none-softfloat",
        "aarch64-unknown-none",
        "i686-unknown-uefi",
        "x86_64-unknown-none",
    ] {
        let mut c = crate::artifacts::cargo(root);
        c.args(["build", "--locked", "--release", "--lib", "--target", target, "--target-dir", "target/cross"])
            .args(&packages);
        step(&format!("bare-metal build, {target}"), &mut c);
    }
    if failed.is_empty() { Ok(()) } else { Err(format!("cross tests failed: {failed:?}")) }
}
