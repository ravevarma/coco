//! Building the loader and kernel, assembling the ESP and disk image,
//! static checks and the reproducibility check.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Res, gpt, output, run};

/// Fixed timestamp for files placed in images (reproducibility):
/// 2026-01-01T00:00:00Z.
pub const SOURCE_DATE_EPOCH: &str = "1767225600";
/// FAT volume serial (fixed for reproducibility).
const FAT_SERIAL: &str = "C0C0C0C0";
/// ESP partition size in MiB.
pub const ESP_MIB: u64 = 64;

/// Paths of everything a build produces.
#[derive(Debug, Clone)]
pub struct Outputs {
    /// `target/coco`.
    pub dir: PathBuf,
    /// Directory tree mirrored into the ESP.
    pub esp_dir: PathBuf,
    /// Loader PE image.
    pub loader: PathBuf,
    /// Kernel ELF.
    pub kernel: PathBuf,
    /// FAT32 ESP filesystem image.
    pub esp_img: PathBuf,
    /// GPT disk image containing the ESP.
    pub disk_img: PathBuf,
}

fn home_dir(var: &str, fallback: &str) -> String {
    std::env::var(var)
        .unwrap_or_else(|_| format!("{}/{fallback}", std::env::var("HOME").unwrap_or_else(|_| "/root".into())))
}

/// Encoded rustflags: target-specific codegen flags plus path remapping so
/// artifacts do not depend on the checkout location or home directory.
/// Code-generation flavour of a bare-metal artifact.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    /// UEFI PE application.
    Uefi,
    /// Kernel: static, linked in the top 2 GiB.
    Kernel,
    /// User program: static, linked low (small code model).
    User,
}

fn rustflags(root: &Path, flavor: Flavor) -> String {
    let mut flags: Vec<String> = Vec::new();
    match flavor {
        Flavor::Kernel => flags.extend(["-C", "relocation-model=static", "-C", "code-model=kernel"].map(String::from)),
        Flavor::User => flags.extend(["-C", "relocation-model=static"].map(String::from)),
        Flavor::Uefi => {}
    }
    flags.extend(remap_flags(root));
    flags.join("\u{1f}")
}

/// Path-remapping flags so artifacts do not depend on the checkout location
/// or home directory.
pub fn remap_flags(root: &Path) -> [String; 3] {
    [
        format!("--remap-path-prefix={}=/coco", root.display()),
        format!("--remap-path-prefix={}=/cargo", home_dir("CARGO_HOME", ".cargo")),
        format!("--remap-path-prefix={}=/rustup", home_dir("RUSTUP_HOME", ".rustup")),
    ]
}

/// A cargo command in `root` that does not inherit flags from the caller.
pub fn cargo(root: &Path) -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root);
    // Never inherit flags from the outer cargo invocation.
    c.env_remove("RUSTFLAGS").env_remove("CARGO_ENCODED_RUSTFLAGS").env_remove("CARGO_TARGET_DIR");
    c
}

fn build_one(root: &Path, package: &str, target: &str, release: bool, flavor: Flavor) -> Res<()> {
    let mut c = cargo(root);
    c.args(["build", "--locked", "-p", package, "--target", target]);
    if release {
        c.arg("--release");
    }
    c.env("CARGO_ENCODED_RUSTFLAGS", rustflags(root, flavor));
    run(&mut c)
}

fn copy(from: &Path, to: &Path) -> Res<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    fs::copy(from, to).map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
    run(Command::new("touch").arg("-d").arg(format!("@{SOURCE_DATE_EPOCH}")).arg(to))
}

/// Creates a FAT32 ESP image from `esp_dir` (512-byte clusters, fixed
/// serial and timestamps).
pub fn make_esp(esp_dir: &Path, img: &Path) -> Res<()> {
    let _ = fs::remove_file(img);
    let f = fs::File::create(img).map_err(|e| format!("create {}: {e}", img.display()))?;
    f.set_len(ESP_MIB << 20).map_err(|e| format!("size {}: {e}", img.display()))?;
    drop(f);
    let mt = |args: &[&str]| -> Res<()> {
        let mut c = Command::new(args.first().copied().unwrap_or("true"));
        c.args(args.get(1..).unwrap_or(&[])).env("SOURCE_DATE_EPOCH", SOURCE_DATE_EPOCH).env("MTOOLS_SKIP_CHECK", "1");
        run(&mut c)
    };
    let img_s = img.to_str().ok_or("non-UTF-8 image path")?;
    mt(&["mformat", "-i", img_s, "-F", "-c", "1", "-N", FAT_SERIAL, "-v", "COCO", "::"])?;
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = esp_dir.join(&rel);
        let mut entries: Vec<_> = fs::read_dir(&dir)
            .map_err(|e| format!("read {}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().ok_or("bad name")?;
            let child = rel.join(name);
            let target = format!("::/{}", child.to_str().ok_or("non-UTF-8 path")?);
            if path.is_dir() {
                mt(&["mmd", "-i", img_s, &target])?;
                stack.push(child);
            } else {
                mt(&["mcopy", "-m", "-o", "-i", img_s, path.to_str().ok_or("non-UTF-8 path")?, &target])?;
            }
        }
    }
    Ok(())
}

fn build_user(root: &Path, release: bool) -> Res<()> {
    let mut c = cargo(root);
    c.args(["build", "--locked", "-p", "coco-init", "--target", "x86_64-unknown-none", "--target-dir", "target/user"]);
    if release {
        c.arg("--release");
    }
    c.env("CARGO_ENCODED_RUSTFLAGS", rustflags(root, Flavor::User));
    run(&mut c)
}

/// Builds loader and kernel, assembles `target/coco/{esp,esp.img,coco.img}`.
pub fn build(root: &Path, release: bool) -> Res<Outputs> {
    build_one(root, "coco-boot", "x86_64-unknown-uefi", release, Flavor::Uefi)?;
    build_one(root, "coco-kernel", "x86_64-unknown-none", release, Flavor::Kernel)?;
    // The user program shares the bare-metal target directory with the
    // kernel but needs different codegen flags; a separate target dir keeps
    // the two builds from invalidating each other.
    build_user(root, release)?;
    let profile = if release { "release" } else { "debug" };
    let target = root.join("target");
    let out = target.join("coco");
    let esp_dir = out.join("esp");
    let _ = fs::remove_dir_all(&esp_dir);
    let loader = esp_dir.join("EFI/BOOT/BOOTX64.EFI");
    let kernel = esp_dir.join("coco/kernel.elf");
    copy(&target.join(format!("x86_64-unknown-uefi/{profile}/coco-boot.efi")), &loader)?;
    copy(&target.join(format!("x86_64-unknown-none/{profile}/coco-kernel")), &kernel)?;
    copy(&target.join(format!("user/x86_64-unknown-none/{profile}/coco-init")), &esp_dir.join("coco/init.elf"))?;
    for d in ["EFI/BOOT", "EFI", "coco"] {
        run(Command::new("touch").arg("-d").arg(format!("@{SOURCE_DATE_EPOCH}")).arg(esp_dir.join(d)))?;
    }
    let esp_img = out.join("esp.img");
    make_esp(&esp_dir, &esp_img)?;
    let disk_img = out.join("coco.img");
    gpt::write_disk(&disk_img, &esp_img)?;
    Ok(Outputs { dir: out, esp_dir, loader, kernel, esp_img, disk_img })
}

/// SHA-256 of a file (via coreutils `sha256sum`).
pub fn sha256(path: &Path) -> Res<String> {
    let out = output(Command::new("sha256sum").arg(path))?;
    out.split_whitespace().next().map(str::to_owned).ok_or_else(|| "sha256sum produced no output".into())
}

/// fmt, clippy (host and both bare-metal targets), tests and docs.
pub fn check(root: &Path) -> Res<()> {
    type Step<'a> = (&'a str, Vec<&'a str>, Vec<(&'a str, &'a str)>);
    let steps: Vec<Step<'_>> = vec![
        ("rustfmt", vec!["fmt", "--all", "--check"], vec![]),
        (
            "clippy (host crates, all targets)",
            vec!["clippy", "--locked", "--all-targets", "--", "-D", "warnings"],
            vec![],
        ),
        (
            "clippy (coco-boot, x86_64-unknown-uefi)",
            vec![
                "clippy",
                "--locked",
                "-p",
                "coco-boot",
                "--target",
                "x86_64-unknown-uefi",
                "--release",
                "--",
                "-D",
                "warnings",
            ],
            vec![],
        ),
        (
            "clippy (coco-init, x86_64-unknown-none)",
            vec![
                "clippy",
                "--locked",
                "-p",
                "coco-init",
                "--target",
                "x86_64-unknown-none",
                "--release",
                "--target-dir",
                "target/user",
                "--",
                "-D",
                "warnings",
            ],
            vec![],
        ),
        (
            "clippy (coco-kernel, x86_64-unknown-none)",
            vec![
                "clippy",
                "--locked",
                "-p",
                "coco-kernel",
                "--target",
                "x86_64-unknown-none",
                "--release",
                "--",
                "-D",
                "warnings",
            ],
            vec![],
        ),
        ("unit + property tests (host)", vec!["test", "--locked"], vec![]),
        ("unit + property tests (host, release)", vec!["test", "--locked", "--release"], vec![]),
        ("rustdoc (host crates)", vec!["doc", "--locked", "--no-deps"], vec![("RUSTDOCFLAGS", "-D warnings")]),
    ];
    // Clippy for the portable kernel on each architecture.
    type OwnedStep = (String, Vec<String>, Vec<(String, String)>);
    let port_steps: Vec<OwnedStep> = crate::ports::PORTS
        .iter()
        .map(|p| {
            let args =
                ["clippy", "--locked", "-p", "coco-port", "--target", p.target, "--release", "--", "-D", "warnings"];
            (
                format!("clippy (coco-port, {})", p.target),
                args.map(String::from).to_vec(),
                vec![("CARGO_ENCODED_RUSTFLAGS".to_string(), crate::ports::port_rustflags(root, p))],
            )
        })
        .collect();
    let steps = steps
        .into_iter()
        .map(|(n, a, e)| {
            (
                n.to_string(),
                a.into_iter().map(String::from).collect::<Vec<_>>(),
                e.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>(),
            )
        })
        .chain(port_steps);
    let mut failed = Vec::new();
    for (name, args, env) in steps {
        println!("==> {name}");
        let mut c = cargo(root);
        c.args(&args);
        for (k, v) in env {
            c.env(k, v);
        }
        match run(&mut c) {
            Ok(()) => println!("    PASS {name}"),
            Err(e) => {
                println!("    FAIL {name}: {e}");
                failed.push(name);
            }
        }
    }
    if failed.is_empty() { Ok(()) } else { Err(format!("checks failed: {failed:?}")) }
}

/// Builds the release twice from two copies of the source tree at different
/// paths and compares artifact hashes.
pub fn reproducibility(root: &Path) -> Res<()> {
    let base = root.join("target/repro");
    let _ = fs::remove_dir_all(&base);
    let mut hashes = Vec::new();
    for name in ["a", "b-different-path"] {
        let copy = base.join(name);
        fs::create_dir_all(&copy).map_err(|e| format!("mkdir: {e}"))?;
        run(Command::new("sh").arg("-c").arg(format!(
            "tar -C '{}' --exclude=./target --exclude=./.git --exclude=./fuzz/target -cf - . | tar -C '{}' -xf -",
            root.display(),
            copy.display()
        )))?;
        let out = build(&copy, true)?;
        let mut h = Vec::new();
        for p in [&out.loader, &out.kernel, &out.esp_img, &out.disk_img] {
            h.push((p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), sha256(p)?));
        }
        for port in crate::ports::PORTS {
            let elf = crate::ports::build_release(&copy, port)?;
            h.push((format!("port-{}", port.name), sha256(&elf)?));
        }
        hashes.push(h);
    }
    let mut ok = true;
    if let [a, b] = hashes.as_slice() {
        for ((name, ha), (_, hb)) in a.iter().zip(b) {
            let same = ha == hb;
            ok &= same;
            println!("{:<14} {} {}", name, if same { "IDENTICAL" } else { "DIFFERENT" }, ha);
            if !same {
                println!("{:<14}           {}", "", hb);
            }
        }
    }
    let _ = fs::remove_dir_all(&base);
    if ok { Ok(()) } else { Err("builds are not bit-for-bit reproducible".into()) }
}
