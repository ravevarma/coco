//! coco build automation.
//!
//! ```text
//! cargo xtask build [--debug]      build loader + kernel, ESP, disk image, size report
//! cargo xtask check                fmt, clippy (host + bare-metal), tests, docs
//! cargo xtask qemu-test [filter]   boot test matrix under QEMU/OVMF
//! cargo xtask run [--cmdline S] [--memory M] [--smp N]   interactive boot (serial on stdio)
//! cargo xtask repro                build twice from separate copies, compare hashes
//! ```

mod artifacts;
mod gpt;
mod qemu;
mod size;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Result type used throughout the tool.
pub type Res<T> = Result<T, String>;

/// Hard ceiling for the installed read-only base system, in bytes.
pub const BASE_SIZE_LIMIT: u64 = 108_000_000;

/// Workspace root (two levels above this crate's manifest).
pub fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().and_then(Path::parent).map_or(manifest.clone(), Path::to_path_buf)
}

/// Runs a command, failing with its exit status.
pub fn run(cmd: &mut Command) -> Res<()> {
    let status = cmd.status().map_err(|e| format!("failed to spawn {cmd:?}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("command failed ({status}): {cmd:?}")) }
}

/// Runs a command and captures stdout.
pub fn output(cmd: &mut Command) -> Res<String> {
    let out = cmd.output().map_err(|e| format!("failed to spawn {cmd:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!("command failed ({}): {cmd:?}\n{}", out.status, String::from_utf8_lossy(&out.stderr)));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("non-UTF-8 output from {cmd:?}: {e}"))
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn real_main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = workspace_root();
    match args.first().map(String::as_str) {
        Some("build") => {
            let release = !args.iter().any(|a| a == "--debug");
            let out = artifacts::build(&root, release)?;
            let report = size::report(&root, &out)?;
            println!("{report}");
            Ok(())
        }
        Some("check") => artifacts::check(&root),
        Some("qemu-test") => {
            let out = artifacts::build(&root, true)?;
            size::report(&root, &out)?;
            qemu::test_matrix(&root, &out, args.get(1).map(String::as_str))
        }
        Some("run") => {
            let out = artifacts::build(&root, true)?;
            let cmdline = arg_value(&args, "--cmdline").unwrap_or_default();
            let memory = arg_value(&args, "--memory").unwrap_or_else(|| "512M".into());
            let smp = arg_value(&args, "--smp").unwrap_or_else(|| "2".into());
            qemu::interactive(&out, &cmdline, &memory, &smp)
        }
        Some("repro") => artifacts::reproducibility(&root),
        _ => {
            Err("usage: cargo xtask <build [--debug] | check | qemu-test [filter] | run [--cmdline S] | repro>".into())
        }
    }
}

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: error: {e}");
            ExitCode::FAILURE
        }
    }
}
