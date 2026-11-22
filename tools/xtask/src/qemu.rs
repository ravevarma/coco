//! QEMU/OVMF boot tests.
//!
//! Every case boots a private copy of the release ESP (with its own
//! `cmdline.txt` or a deliberately corrupted kernel) in a fresh QEMU with
//! fresh OVMF variables, captures COM1 to a file and checks:
//! * the QEMU exit status (`isa-debug-exit`: 33 = kernel success,
//!   35 = kernel fatal stop), or that a terminal line appeared;
//! * required and forbidden serial output;
//! * for screenshot cases, framebuffer pixels via a QMP `screendump`.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::artifacts::{Outputs, make_esp};
use crate::{Res, gpt, run};

const OVMF_CODE: &str = "/usr/share/OVMF/OVMF_CODE_4M.fd";
const OVMF_VARS: &str = "/usr/share/OVMF/OVMF_VARS_4M.fd";
const EXIT_SUCCESS: i32 = 33;
const EXIT_FATAL: i32 = 35;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Disk {
    Nvme,
    Ahci,
}

#[derive(Clone, Copy)]
enum Payload {
    Normal,
    /// Kernel ELF truncated to this many bytes.
    TruncatedKernel(usize),
    /// Kernel ELF absent.
    NoKernel,
    /// Raw bytes for cmdline.txt (bypasses the normal ASCII path).
    RawCmdline(&'static [u8]),
}

struct Case {
    name: &'static str,
    cmdline: Option<&'static str>,
    memory: &'static str,
    smp: u32,
    cpu: &'static str,
    /// Let the guest reboot (no `-no-reboot`).
    allow_reboot: bool,
    disk: Disk,
    payload: Payload,
    /// Expected QEMU exit status, or `None` = run until `until` appears.
    exit: Option<i32>,
    until: &'static str,
    must: &'static [&'static str],
    must_not: &'static [&'static str],
    screenshot: bool,
    repeat: u32,
}

const NO_ERRORS: &[&str] = &["FATAL", "PANIC", "FAIL"];

const fn case(
    name: &'static str,
    cmdline: &'static str,
    exit: i32,
    must: &'static [&'static str],
    must_not: &'static [&'static str],
) -> Case {
    Case {
        name,
        cmdline: if cmdline.is_empty() { None } else { Some(cmdline) },
        memory: "256M",
        smp: 1,
        cpu: "max",
        disk: Disk::Nvme,
        payload: Payload::Normal,
        exit: Some(exit),
        until: "",
        must,
        must_not,
        screenshot: false,
        repeat: 1,
        allow_reboot: false,
    }
}

fn cases() -> Vec<Case> {
    let selftest_ok: &[&str] = &[
        "selftest memory suite: 4 passed, 0 failed",
        "selftest breakpoint: PASS",
        "selftest heap: PASS",
        "selftest timer: PASS",
        "interrupts on",
        "coco: boot complete",
    ];
    vec![
        Case {
            name: "boot-default",
            cmdline: None,
            exit: None,
            until: "coco: boot complete",
            must: &[
                "coco-boot 0.1.0 starting",
                "framebuffer: 1280x800 ready",
                "mm: loader identity map removed",
                "security: NX on",
                "SMEP on, SMAP on",
                "IST guards on",
                "acpi: MADT 1 CPUs (1 enabled)",
                "calibrated against ACPI PM timer",
                "init: hello from user space, pid 1",
                "init: started as pid 1, exited with Some(Exited(0))",
            ],
            must_not: NO_ERRORS,
            screenshot: true,
            ..case("", "", 0, &[], &[])
        },
        Case { repeat: 3, ..case("selftest-256M", "test=memory qemu-exit", EXIT_SUCCESS, selftest_ok, NO_ERRORS) },
        Case { memory: "4G", ..case("selftest-4G", "test=memory qemu-exit", EXIT_SUCCESS, selftest_ok, NO_ERRORS) },
        Case {
            smp: 4,
            memory: "1G",
            ..case("selftest-smp4-1G", "test=memory qemu-exit", EXIT_SUCCESS, selftest_ok, NO_ERRORS)
        },
        Case {
            smp: 4,
            ..case(
                "acpi-smp4",
                "test=breakpoint qemu-exit",
                EXIT_SUCCESS,
                &["acpi: MADT 4 CPUs (4 enabled), 1 I/O APIC(s)"],
                NO_ERRORS,
            )
        },
        Case {
            cpu: "qemu64",
            ..case(
                "selftest-cpu-qemu64",
                "test=memory qemu-exit",
                EXIT_SUCCESS,
                &["selftest memory suite: 4 passed, 0 failed", "selftest timer: PASS", "SMEP off, SMAP off"],
                NO_ERRORS,
            )
        },
        Case {
            cpu: "max,+x2apic",
            ..case(
                // QEMU 8.2 TCG does not implement x2APIC, so this verifies the
                // refusal/fallback path; the x2APIC register path itself is
                // unverified (see docs/quality-report.md).
                "x2apic-unsupported-fallback",
                "test=memory x2apic qemu-exit",
                EXIT_SUCCESS,
                &[
                    "x2APIC requested but the CPU does not report it",
                    "xAPIC mode; interrupts on",
                    "selftest timer: PASS",
                    "selftest memory suite: 4 passed, 0 failed",
                ],
                NO_ERRORS,
            )
        },
        case(
            "threads",
            "test=threads qemu-exit",
            EXIT_SUCCESS,
            &["selftest threads: PASS", "300 spawn/join ok"],
            NO_ERRORS,
        ),
        case(
            "user-isolation",
            "test=user qemu-exit",
            EXIT_SUCCESS,
            &[
                "selftest user: PASS",
                "user isolation: null dereference: killed",
                "user isolation: kernel memory read: killed",
                "user isolation: privileged instruction: killed",
                "user isolation: execute stack (NX): killed",
                "user isolation: write to text (W^X): killed",
                "user isolation: user stack overflow: killed",
                "init: syscall fuzz done",
                "120 processes churned",
            ],
            NO_ERRORS,
        ),
        Case {
            cpu: "qemu64",
            ..case(
                "user-isolation-no-smap",
                "test=user qemu-exit",
                EXIT_SUCCESS,
                &["SMEP off, SMAP off", "selftest user: PASS", "user isolation: kernel memory read: killed"],
                NO_ERRORS,
            )
        },
        case(
            "soak-60s",
            "test=soak secs=60 qemu-exit",
            EXIT_SUCCESS,
            &["selftest soak: PASS", "frames/heap at baseline"],
            &["FATAL", "PANIC", "FAIL", "not back to baseline"],
        ),
        case(
            "ipc-capabilities",
            "test=ipc qemu-exit",
            EXIT_SUCCESS,
            &[
                "selftest ipc: PASS",
                "init: ipc echo: 1000 round trips verified",
                "init: capability tests: 21 checks passed",
                "live channels 0",
            ],
            NO_ERRORS,
        ),
        case(
            "bench-ipc",
            "test=bench qemu-exit",
            EXIT_SUCCESS,
            &["selftest bench: PASS", "syscall bench:", "yield bench:", "ipc echo: 1000 round trips verified"],
            NO_ERRORS,
        ),
        case("timer", "test=timer qemu-exit", EXIT_SUCCESS, &["selftest timer: PASS", "100 ticks took"], NO_ERRORS),
        Case {
            memory: "512M",
            ..case(
                "heap-stress-exhaustion",
                "test=heap qemu-exit",
                EXIT_SUCCESS,
                &["selftest heap: PASS", "selftest heap-exhaustion: PASS", "before OOM"],
                NO_ERRORS,
            )
        },
        case(
            "irq-alloc-refused",
            "test=irq-alloc qemu-exit",
            EXIT_FATAL,
            &["heap: allocation of 8 bytes in interrupt context refused", "memory allocation of 8 bytes failed"],
            &["was not refused"],
        ),
        Case {
            allow_reboot: true,
            ..case(
                "lifecycle-3-boots",
                "test=lifecycle cycles=3 qemu-exit",
                0,
                &[
                    "lifecycle: boot 1 of 3",
                    "lifecycle: boot 2 of 3",
                    "lifecycle: boot 3 of 3",
                    "power: reboot via ACPI reset register",
                    "lifecycle: 3 boots complete",
                    "power: entering ACPI S5",
                ],
                &["FATAL", "PANIC", "FAIL"],
            )
        },
        case("shutdown", "test=shutdown qemu-exit", 0, &["power: entering ACPI S5 (SLP_TYP"], &["FAIL"]),
        Case {
            disk: Disk::Ahci,
            ..case("selftest-ahci", "test=memory qemu-exit", EXIT_SUCCESS, selftest_ok, NO_ERRORS)
        },
        case(
            "fault-breakpoint",
            "test=breakpoint qemu-exit",
            EXIT_SUCCESS,
            &["breakpoint at", "selftest breakpoint: PASS"],
            NO_ERRORS,
        ),
        case(
            "fault-pagefault",
            "test=pagefault qemu-exit",
            EXIT_FATAL,
            &[
                "FATAL: PAGE FAULT (#PF) (vector 14)",
                "fault address 0xfffffd0050000000: page not present on read in kernel mode",
            ],
            &["expected fault did not occur"],
        ),
        case(
            "fault-null",
            "test=null qemu-exit",
            EXIT_FATAL,
            &["FATAL: PAGE FAULT", "fault address 0x10: page not present on read"],
            &["expected fault did not occur"],
        ),
        case(
            "fault-with-console-locked",
            "test=locked-fault qemu-exit",
            EXIT_FATAL,
            &["FATAL: PAGE FAULT", "fault address 0xfffffd0060000000: page not present"],
            &["expected fault did not occur"],
        ),
        case(
            "fault-guard-page",
            "test=guard qemu-exit",
            EXIT_FATAL,
            &["FATAL: PAGE FAULT", "cause: kernel stack overflow (guard page"],
            &["expected fault did not occur"],
        ),
        case(
            "fault-stack-overflow",
            "test=stack-overflow qemu-exit",
            EXIT_FATAL,
            &["FATAL: DOUBLE FAULT (#DF) (vector 8)", "cause: kernel stack overflow (guard page"],
            &["expected fault did not occur"],
        ),
        case(
            "fault-nx",
            "test=nx qemu-exit",
            EXIT_FATAL,
            &["FATAL: PAGE FAULT", "protection violation on instruction fetch in kernel mode"],
            &["expected fault did not occur"],
        ),
        case(
            "fault-write-protect",
            "test=wp qemu-exit",
            EXIT_FATAL,
            &["FATAL: PAGE FAULT", "protection violation on write in kernel mode"],
            &["expected fault did not occur"],
        ),
        case(
            "fault-invalid-opcode",
            "test=ud qemu-exit",
            EXIT_FATAL,
            &["FATAL: invalid opcode (#UD) (vector 6)"],
            &["expected fault did not occur"],
        ),
        case(
            "fault-divide",
            "test=divide qemu-exit",
            EXIT_FATAL,
            &["FATAL: divide error (#DE) (vector 0)"],
            &["expected fault did not occur"],
        ),
        case(
            "fault-panic",
            "test=panic qemu-exit",
            EXIT_FATAL,
            &["KERNEL PANIC: ", "deliberate test panic"],
            &["expected fault did not occur"],
        ),
        case(
            "unknown-test",
            "test=nonexistent qemu-exit",
            EXIT_FATAL,
            &["selftest nonexistent: FAIL: unknown test", "self-test FAILURE"],
            &[],
        ),
        Case {
            payload: Payload::RawCmdline(b"test=memory \xff\xfe qemu-exit"),
            exit: None,
            until: "coco-boot: FATAL: BadCmdline",
            ..case("reject-bad-cmdline", "", 0, &["coco-boot: FATAL: BadCmdline"], &["coco kernel"])
        },
        Case {
            payload: Payload::TruncatedKernel(4000),
            exit: None,
            until: "coco-boot: FATAL: Elf(",
            ..case("reject-truncated-kernel", "", 0, &["coco-boot: FATAL: Elf("], &["coco kernel"])
        },
        Case {
            payload: Payload::NoKernel,
            exit: None,
            until: "coco-boot: FATAL: MissingKernel",
            ..case("reject-missing-kernel", "", 0, &["coco-boot: FATAL: MissingKernel"], &["coco kernel"])
        },
    ]
}

/// Removes ANSI escape sequences and carriage returns from firmware output.
fn clean(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

fn prepare_case(out: &Outputs, case: &Case, dir: &Path) -> Res<PathBuf> {
    let esp_dir = dir.join("esp");
    let _ = fs::remove_dir_all(&esp_dir);
    run(Command::new("cp").arg("-a").arg(&out.esp_dir).arg(&esp_dir))?;
    let kernel = esp_dir.join("coco/kernel.elf");
    match case.payload {
        Payload::Normal => {}
        Payload::TruncatedKernel(n) => {
            let data = fs::read(&kernel).map_err(|e| format!("read kernel: {e}"))?;
            fs::write(&kernel, data.get(..n).unwrap_or(&data)).map_err(|e| format!("write kernel: {e}"))?;
        }
        Payload::NoKernel => fs::remove_file(&kernel).map_err(|e| format!("rm kernel: {e}"))?,
        Payload::RawCmdline(bytes) => {
            fs::write(esp_dir.join("coco/cmdline.txt"), bytes).map_err(|e| format!("cmdline: {e}"))?
        }
    }
    if let Some(cmd) = case.cmdline {
        fs::write(esp_dir.join("coco/cmdline.txt"), cmd).map_err(|e| format!("cmdline: {e}"))?;
    }
    let esp = dir.join("esp.img");
    make_esp(&esp_dir, &esp)?;
    let disk = dir.join("disk.img");
    gpt::write_disk(&disk, &esp)?;
    let _ = fs::remove_file(&esp);
    Ok(disk)
}

struct Machine<'a> {
    memory: &'a str,
    smp: u32,
    cpu: &'a str,
    ahci: bool,
    allow_reboot: bool,
}

fn qemu_command(disk: &Path, dir: &Path, m: &Machine<'_>, serial: Option<&Path>, qmp: Option<&Path>) -> Res<Command> {
    let vars = dir.join("OVMF_VARS.fd");
    fs::copy(OVMF_VARS, &vars).map_err(|e| format!("copy OVMF vars ({OVMF_VARS}): {e}"))?;
    let mut c = Command::new("qemu-system-x86_64");
    c.args(["-machine", "q35", "-cpu", m.cpu, "-m", m.memory, "-smp", &m.smp.to_string()]);
    if !m.allow_reboot {
        c.arg("-no-reboot");
    }
    c.arg("-drive").arg(format!("if=pflash,format=raw,readonly=on,file={OVMF_CODE}"));
    c.arg("-drive").arg(format!("if=pflash,format=raw,file={}", vars.display()));
    c.arg("-drive").arg(format!("file={},format=raw,if=none,id=disk0", disk.display()));
    if m.ahci {
        c.args(["-device", "ide-hd,drive=disk0,bus=ide.0"]);
    } else {
        c.args(["-device", "nvme,serial=coco0,drive=disk0"]);
    }
    c.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04", "-net", "none"]);
    match serial {
        Some(path) => {
            c.args(["-display", "none", "-serial"]).arg(format!("file:{}", path.display()));
        }
        None => {
            c.args(["-display", "none", "-serial", "stdio"]);
        }
    }
    if let Some(sock) = qmp {
        c.arg("-qmp").arg(format!("unix:{},server=on,wait=off", sock.display()));
    }
    Ok(c)
}

fn qmp_screendump(sock: &Path, file: &Path, format: &str) -> Res<()> {
    let stream = UnixStream::connect(sock).map_err(|e| format!("qmp connect: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(20))).map_err(|e| format!("qmp timeout: {e}"))?;
    let mut writer = stream.try_clone().map_err(|e| format!("qmp clone: {e}"))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let expect_return = |reader: &mut BufReader<UnixStream>, line: &mut String| -> Res<()> {
        loop {
            line.clear();
            if reader.read_line(line).map_err(|e| format!("qmp read: {e}"))? == 0 {
                return Err("qmp closed".into());
            }
            if line.contains("\"error\"") {
                return Err(format!("qmp error: {line}"));
            }
            if line.contains("\"return\"") {
                return Ok(());
            }
        }
    };
    reader.read_line(&mut line).map_err(|e| format!("qmp greeting: {e}"))?;
    writer.write_all(b"{\"execute\":\"qmp_capabilities\"}\n").map_err(|e| format!("qmp write: {e}"))?;
    expect_return(&mut reader, &mut line)?;
    let cmd = format!(
        "{{\"execute\":\"screendump\",\"arguments\":{{\"filename\":\"{}\",\"format\":\"{format}\"}}}}\n",
        file.display()
    );
    writer.write_all(cmd.as_bytes()).map_err(|e| format!("qmp write: {e}"))?;
    expect_return(&mut reader, &mut line)
}

/// Parses a binary PPM (P6) image into (width, height, rgb bytes).
fn parse_ppm(data: &[u8]) -> Res<(usize, usize, &[u8])> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while data.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let start = i;
        while data.get(i).is_some_and(|b| !b.is_ascii_whitespace()) {
            i += 1;
        }
        fields.push(std::str::from_utf8(data.get(start..i).ok_or("ppm header")?).map_err(|_| "ppm header")?.to_owned());
    }
    let w: usize = fields.get(1).and_then(|s| s.parse().ok()).ok_or("ppm width")?;
    let h: usize = fields.get(2).and_then(|s| s.parse().ok()).ok_or("ppm height")?;
    if fields.first().map(String::as_str) != Some("P6") || fields.get(3).map(String::as_str) != Some("255") {
        return Err("unsupported ppm".into());
    }
    let pixels = data.get(i + 1..i + 1 + w * h * 3).ok_or("ppm truncated")?;
    Ok((w, h, pixels))
}

/// Verifies the boot splash: background, coconut mark, wordmark, full
/// progress bar and console text must all be present in their colours.
fn check_splash(ppm: &Path) -> Res<String> {
    let data = fs::read(ppm).map_err(|e| format!("read screendump: {e}"))?;
    let (w, h, px) = parse_ppm(&data)?;
    let at = |x: usize, y: usize| -> u32 {
        let o = (y * w + x) * 3;
        px.get(o..o + 3).map_or(0, |p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
    };
    let count = |rgb: u32| (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| at(x, y) == rgb).count();
    let (bg, shell, cream, amber, text) = (0x14161D, 0x8A5733, 0xF3E9D2, 0xE8A33D, 0xC9CED8);
    let mut problems = Vec::new();
    if at(2, 2) != bg {
        problems.push(format!("corner pixel {:06x} != background", at(2, 2)));
    }
    let r = w.min(h) / 10;
    if at(w / 2, h * 3 / 10 + r / 2) != shell {
        problems.push(format!("logo pixel {:06x} != shell colour", at(w / 2, h * 3 / 10 + r / 2)));
    }
    let n_bg = count(bg);
    if n_bg * 2 < w * h {
        problems.push(format!("background covers only {n_bg} px"));
    }
    for (name, rgb) in [("wordmark", cream), ("progress bar", amber), ("console text", text), ("shell", shell)] {
        if count(rgb) == 0 {
            problems.push(format!("no {name} pixels"));
        }
    }
    let progress = count(amber);
    if problems.is_empty() {
        Ok(format!("{w}x{h}, background {n_bg} px, progress bar {progress} px"))
    } else {
        Err(problems.join("; "))
    }
}

struct Outcome {
    pass: bool,
    detail: String,
    seconds: f64,
    kernel_entry_s: Option<f64>,
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn run_case(root: &Path, out: &Outputs, case: &Case, iteration: u32) -> Res<Outcome> {
    let dir = out.dir.join("qemu").join(case.name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let disk = prepare_case(out, case, &dir)?;
    let serial = dir.join("serial.log");
    let qmp = case.screenshot.then(|| dir.join("qmp.sock"));
    let machine = Machine {
        memory: case.memory,
        smp: case.smp,
        cpu: case.cpu,
        ahci: case.disk == Disk::Ahci,
        allow_reboot: case.allow_reboot,
    };
    let mut cmd = qemu_command(&disk, &dir, &machine, Some(&serial), qmp.as_deref())?;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("spawn qemu: {e}"))?;
    let timeout = Duration::from_secs(180);
    let mut kernel_entry = None;
    let mut status = None;
    let mut extra = String::new();
    loop {
        std::thread::sleep(Duration::from_millis(25));
        let log = clean(&fs::read(&serial).unwrap_or_default());
        if kernel_entry.is_none() && log.contains("coco kernel ") {
            kernel_entry = Some(start.elapsed().as_secs_f64());
        }
        if let Some(st) = child.try_wait().map_err(|e| format!("wait: {e}"))? {
            status = st.code();
            break;
        }
        if case.exit.is_none() && log.contains(case.until) {
            if let Some(sock) = &qmp {
                // Give the guest a moment to finish drawing after the line.
                std::thread::sleep(Duration::from_millis(300));
                let ppm = dir.join("screen.ppm");
                let png = root.join("docs/images/boot-splash.png");
                let shot = qmp_screendump(sock, &ppm, "ppm").and_then(|()| check_splash(&ppm)).and_then(|s| {
                    if iteration == 0 {
                        fs::create_dir_all(root.join("docs/images")).map_err(|e| format!("mkdir: {e}"))?;
                        qmp_screendump(sock, &png, "png")?;
                    }
                    Ok(s)
                });
                match shot {
                    Ok(s) => extra = format!("screen ok: {s}"),
                    Err(e) => {
                        kill(&mut child);
                        return Ok(Outcome {
                            pass: false,
                            detail: format!("screenshot check failed: {e}"),
                            seconds: start.elapsed().as_secs_f64(),
                            kernel_entry_s: kernel_entry,
                        });
                    }
                }
            }
            kill(&mut child);
            break;
        }
        if start.elapsed() > timeout {
            kill(&mut child);
            return Ok(Outcome {
                pass: false,
                detail: "timeout".into(),
                seconds: start.elapsed().as_secs_f64(),
                kernel_entry_s: kernel_entry,
            });
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    let log = clean(&fs::read(&serial).unwrap_or_default());
    fs::write(dir.join("serial.clean.log"), &log).map_err(|e| format!("write log: {e}"))?;
    let mut problems = Vec::new();
    if let Some(want) = case.exit {
        if status != Some(want) {
            problems.push(format!("exit status {status:?}, expected {want}"));
        }
    } else if !log.contains(case.until) {
        problems.push(format!("never saw {:?}", case.until));
    }
    for m in case.must {
        if !log.contains(m) {
            problems.push(format!("missing {m:?}"));
        }
    }
    for m in case.must_not {
        if log.contains(m) {
            problems.push(format!("forbidden {m:?} present"));
        }
    }
    let pass = problems.is_empty();
    let detail = if pass { extra } else { problems.join("; ") };
    Ok(Outcome { pass, detail, seconds, kernel_entry_s: kernel_entry })
}

/// Runs the boot matrix and writes `target/coco/qemu-results.md`.
pub fn test_matrix(root: &Path, out: &Outputs, filter: Option<&str>) -> Res<()> {
    which("qemu-system-x86_64")?;
    for f in [OVMF_CODE, OVMF_VARS] {
        if !Path::new(f).exists() {
            return Err(format!("{f} not found (install the ovmf package)"));
        }
    }
    let version = crate::output(Command::new("qemu-system-x86_64").arg("--version"))?;
    let mut table = String::new();
    let _ = writeln!(table, "# QEMU boot test results\n\n{}\n", version.lines().next().unwrap_or("").trim());
    let _ = writeln!(
        table,
        "| Case | Run | Result | Wall time (s) | Kernel entry (s) | Detail |\n|---|---:|---|---:|---:|---|"
    );
    let (mut passed, mut failed) = (0, 0);
    for case in cases().iter().filter(|c| filter.is_none_or(|f| c.name.contains(f))) {
        for i in 0..case.repeat {
            let o = run_case(root, out, case, i)?;
            let verdict = if o.pass { "PASS" } else { "FAIL" };
            let entry = o.kernel_entry_s.map_or_else(|| "-".into(), |s| format!("{s:.2}"));
            println!("{verdict}  {:<26} run {} {:>6.2}s  {}", case.name, i + 1, o.seconds, o.detail);
            let _ = writeln!(
                table,
                "| {} | {} | {verdict} | {:.2} | {entry} | {} |",
                case.name,
                i + 1,
                o.seconds,
                o.detail
            );
            if o.pass { passed += 1 } else { failed += 1 }
        }
    }
    let _ = writeln!(
        table,
        "\n**{passed} passed, {failed} failed.** Times are host wall-clock under TCG emulation (no KVM); they are not representative of hardware boot time."
    );
    fs::write(out.dir.join("qemu-results.md"), &table).map_err(|e| format!("write results: {e}"))?;
    println!("\n{passed} passed, {failed} failed (details: target/coco/qemu/<case>/serial.clean.log)");
    if failed == 0 { Ok(()) } else { Err(format!("{failed} QEMU case(s) failed")) }
}

fn which(tool: &str) -> Res<()> {
    run(Command::new("sh").arg("-c").arg(format!("command -v {tool} >/dev/null")))
        .map_err(|_| format!("{tool} not installed"))
}

/// Boots the release image with serial on stdio.
pub fn interactive(out: &Outputs, cmdline: &str, memory: &str, smp: &str) -> Res<()> {
    let dir = out.dir.join("qemu").join("interactive");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    // One-shot CLI process: leaking the small command-line string is fine.
    let cmdline: &'static str = Box::leak(cmdline.to_owned().into_boxed_str());
    let case = Case { cmdline: Some(cmdline), ..case("interactive", "", 0, &[], &[]) };
    let disk = prepare_case(out, &case, &dir)?;
    let smp: u32 = smp.parse().map_err(|_| "bad --smp")?;
    let machine = Machine { memory, smp, cpu: "max", ahci: false, allow_reboot: true };
    run(&mut qemu_command(&disk, &dir, &machine, None, None)?)
}
