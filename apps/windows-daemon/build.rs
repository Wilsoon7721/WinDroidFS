use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../packages/binary-protocol/windroid.proto");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    prost_build::Config::new()
        .out_dir(&out_dir)
        .compile_protos(
            &["../../packages/binary-protocol/windroid.proto"],
            &["../../packages/binary-protocol/"],
        )
        .unwrap();

    embed_icon(&out_dir);
}

/// Compile assets/icon.rc and hand the result to the linker, so both .exe files carry the
/// WinDroidFS icon in Explorer, the taskbar and Alt+Tab.
///
/// This uses rc.exe from the Windows SDK rather than a crate, so it adds no dependency. If the
/// SDK isn't installed the build still succeeds — you just get the default blank exe icon.
fn embed_icon(out_dir: &Path) {
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/windroidfs.ico");
    println!("cargo:rerun-if-env-changed=WINDROIDFS_RC");

    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return; // rc.exe is MSVC-only; other toolchains just skip the icon
    }

    let Some(rc) = find_rc_exe() else {
        println!("cargo:warning=rc.exe not found; building without an exe icon");
        return;
    };

    let res = out_dir.join("icon.res");
    let status = Command::new(&rc)
        .args(["/nologo", "/fo"])
        .arg(&res)
        .arg("assets/icon.rc")
        .status();

    match status {
        Ok(s) if s.success() => {
            // Applies to the binaries only; the lib target has nothing to link an icon into.
            println!("cargo:rustc-link-arg-bins={}", res.display());
        }
        Ok(s) => println!("cargo:warning=rc.exe failed ({s}); building without an exe icon"),
        Err(e) => {
            println!("cargo:warning=could not run rc.exe ({e}); building without an exe icon")
        }
    }
}

/// `$WINDROIDFS_RC` wins if set; otherwise take the newest Windows SDK's x64 rc.exe.
fn find_rc_exe() -> Option<PathBuf> {
    if let Some(explicit) = env::var_os("WINDROIDFS_RC") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }

    let mut candidates: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(env::var_os)
        .map(|root| PathBuf::from(root).join("Windows Kits/10/bin"))
        .filter_map(|bin| std::fs::read_dir(bin).ok())
        .flatten()
        .filter_map(|entry| entry.ok())
        // SDK versions are directories like "10.0.26100.0"
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("10."))
        .map(|entry| entry.path())
        .collect();

    // Directory order isn't guaranteed; sorting by name puts the newest SDK last.
    candidates.sort();
    candidates
        .iter()
        .rev()
        .map(|version_dir| version_dir.join("x64/rc.exe"))
        .find(|rc| rc.is_file())
}
