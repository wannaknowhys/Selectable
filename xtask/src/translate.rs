// `cargo xtask build-translate`: hermetic Bergamot native build.
// Owns: vcpkg bootstrap (build-native/vcpkg), MKL static fetch, vcvars
// discovery, cmake configure+build of the bt_shim static lib. Same path
// locally and on CI; never touches global env or settings.
use std::path::{Path, PathBuf};

const MKL_URL: &str =
    "https://data.statmt.org/romang/marian-regression-tests/ci/mkl-2020.1-windows-static.zip";
const MKL_ZIP_BYTES: u64 = 205_409_991;
const VCPKG_URL: &str = "https://github.com/microsoft/vcpkg.git";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn run(cmd: &str, args: &[&str], dir: &Path, extra_env: &[(&str, &str)]) -> anyhow::Result<()> {
    println!("[translate] {cmd} {}", args.join(" "));
    let mut c = std::process::Command::new(cmd);
    c.args(args).current_dir(dir);
    for (k, v) in extra_env {
        c.env(k, v);
    }
    let st = c.status()?;
    if !st.success() {
        anyhow::bail!("command failed: {cmd} {}", args.join(" "));
    }
    Ok(())
}

/// Same but captures output; prints it only on failure (for fast opaque steps
/// like cmake configure; long builds keep streaming via run()).
fn run_capture(cmd: &str, args: &[&str], dir: &Path, extra_env: &[(&str, &str)]) -> anyhow::Result<()> {
    println!("[translate] {cmd} {}", args.join(" "));
    let mut c = std::process::Command::new(cmd);
    c.args(args).current_dir(dir);
    for (k, v) in extra_env {
        c.env(k, v);
    }
    let out = c.output()?;
    if !out.status.success() {
        eprintln!("--- stdout ---\n{}", String::from_utf8_lossy(&out.stdout));
        eprintln!("--- stderr ---\n{}", String::from_utf8_lossy(&out.stderr));
        anyhow::bail!("command failed: {cmd} {}", args.join(" "));
    }
    Ok(())
}

/// Apply a carried patch to the submodule working tree, idempotently.
/// (Upstream pins can't take our Windows/Rust-compat tweaks any other way;
///
/// this keeps the diff reviewable under native/patches/ instead of a fork.)
fn apply_patch_once(submodule: &Path, patch: &Path, root: &Path) -> anyhow::Result<()> {
    let sub = submodule.to_string_lossy().into_owned();
    let p = patch.to_string_lossy().into_owned();
    // Already applied? `apply --check --reverse` succeeds only then.
    let rev = std::process::Command::new("git")
        .args(["-C", &sub, "apply", "--check", "--reverse", &p])
        .current_dir(root)
        .status()?;
    if rev.success() {
        println!("[translate] patch already applied");
        return Ok(());
    }
    run("git", &["-C", &sub, "apply", &p], root, &[])?;
    println!("[translate] patch applied");
    Ok(())
}
/// Locate cmake: SELECTABLE_CMAKE_DIR (dir holding cmake+ninja) > PATH >
/// standard install dir. Returns (cmake_path, optional PATH prepend for kids).
fn cmake_tool() -> anyhow::Result<(String, Option<String>)> {
    if let Ok(dir) = std::env::var("SELECTABLE_CMAKE_DIR") {
        let dir = dir.trim().trim_matches('"').to_string();
        let exe = PathBuf::from(&dir).join("cmake.exe");
        if exe.is_file() {
            return Ok((exe.to_string_lossy().into_owned(), Some(dir)));
        }
        anyhow::bail!("SELECTABLE_CMAKE_DIR={dir} has no cmake.exe");
    }
    if let Ok(path) = std::env::var("PATH") {
        for d in path.split(';') {
            if PathBuf::from(d).join("cmake.exe").is_file() {
                return Ok(("cmake".to_string(), None));
            }
        }
    }
    let std = "C:/Program Files/CMake/bin/cmake.exe";
    if PathBuf::from(std).is_file() {
        return Ok((std.to_string(), None));
    }
    anyhow::bail!("cmake not found: put it on PATH or set SELECTABLE_CMAKE_DIR to its bin dir")
}

fn find_vcvars() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("VSINSTALLDIR") {
        let p = PathBuf::from(dir).join("VC/Auxiliary/Build/vcvars64.bat");
        if p.is_file() {
            return Ok(p);
        }
    }
    for cand in [
        "C:/Program Files/Microsoft Visual Studio/18/Community/VC/Auxiliary/Build/vcvars64.bat",
        "C:/Program Files/Microsoft Visual Studio/2022/Community/VC/Auxiliary/Build/vcvars64.bat",
        "C:/Program Files/Microsoft Visual Studio/2022/BuildTools/VC/Auxiliary/Build/vcvars64.bat",
        "C:/Program Files (x86)/Microsoft Visual Studio/2019/BuildTools/VC/Auxiliary/Build/vcvars64.bat",
    ] {
        let p = PathBuf::from(cand);
        if p.is_file() {
            return Ok(p);
        }
    }
    anyhow::bail!("vcvars64.bat not found (set VSINSTALLDIR or install VS Build Tools)")
}

/// Trampoline .bat: enters the MSVC env, then runs whatever argv follows.
/// Batch parsing handles spacey paths fine (cmd /C inline quoting does not).
fn msvc_trampoline(dir: &Path, vcvars: &Path) -> anyhow::Result<PathBuf> {
    let bat = dir.join("build-native/_msvcenv.bat");
    let vc = vcvars.to_string_lossy().replace('/', "\\");
    let body = format!("@echo off\r\ncall \"{vc}\" >nul\r\n%*\r\n");
    let cur = std::fs::read_to_string(&bat).unwrap_or_default();
    if cur != body {
        if let Some(p) = bat.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&bat, body)?;
    }
    Ok(bat)
}

/// Run a program inside the MSVC x64 environment without touching global
/// state. The program + args travel as SEPARATE argv entries (each quoted
/// correctly by Rust); only the trampoline path itself is quoted by cmd.
fn run_in_msvc(
    vcvars: &Path,
    program: &str,
    args: &[String],
    dir: &Path,
    extra_env: &[(&str, &str)],
) -> anyhow::Result<()> {
    let bat = msvc_trampoline(dir, vcvars)?;
    let mut argv: Vec<String> =
        vec!["/C".to_string(), bat.to_string_lossy().into_owned(), program.to_string()];
    argv.extend(args.iter().cloned());
    let argv_ref: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    run("cmd", &argv_ref, dir, extra_env)
}

fn run_in_msvc_capture(
    vcvars: &Path,
    program: &str,
    args: &[String],
    dir: &Path,
    extra_env: &[(&str, &str)],
) -> anyhow::Result<()> {
    let bat = msvc_trampoline(dir, vcvars)?;
    let mut argv: Vec<String> =
        vec!["/C".to_string(), bat.to_string_lossy().into_owned(), program.to_string()];
    argv.extend(args.iter().cloned());
    let argv_ref: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    run_capture("cmd", &argv_ref, dir, extra_env)
}

pub fn build_translate() -> anyhow::Result<()> {
    let root = root();
    let bn = root.join("build-native");
    std::fs::create_dir_all(&bn)?;

    // 1. vcpkg (hermetic copy inside build-native; CI may take minutes once).
    let vcpkg_dir = bn.join("vcpkg");
    let vcpkg_exe = vcpkg_dir.join("vcpkg.exe");
    if !vcpkg_exe.is_file() {
        println!("[translate] cloning vcpkg (one-time) ...");
        run("git", &["clone", "--depth", "1", VCPKG_URL, &vcpkg_dir.to_string_lossy()], &root, &[])?;
        run(
            vcpkg_dir.join("bootstrap-vcpkg.bat").to_str().unwrap(),
            &["-disableMetrics"],
            &root,
            &[],
        )?;
    }
    // 2. pcre2 static (the only vcpkg port we need; Qt/GUI intentionally skipped).
    run(
        vcpkg_exe.to_str().unwrap(),
        &["install", "pcre2:x64-windows-static"],
        &root,
        &[("VCPKG_BUILD_TYPE", "release"), ("VCPKG_DEFAULT_TRIPLET", "x64-windows-static")],
    )?;

    // 3. MKL static (pinned size; exe-dir stays clean, linked statically).
    let mkl = bn.join("mkl");
    let mkl_h = mkl.join("include/mkl.h");
    if !mkl_h.is_file() {
        println!("[translate] fetching MKL static ...");
        let zip = bn.join("mkl.zip");
        run("curl.exe", &["-L", "-o", &zip.to_string_lossy(), MKL_URL], &root, &[])?;
        let len = std::fs::metadata(&zip)?.len();
        if len != MKL_ZIP_BYTES {
            anyhow::bail!("MKL zip size mismatch: got {len}, want {MKL_ZIP_BYTES}");
        }
        std::fs::create_dir_all(&mkl)?;
        run("tar.exe", &["-xf", &zip.to_string_lossy(), "-C", &mkl.to_string_lossy()], &root, &[])?;
        // tar extracts include/ + lib/ into mkl/ (strip the top level if nested).
        if !mkl_h.is_file() {
            // Handle one-level nesting defensively.
            if let Ok(entries) = std::fs::read_dir(&mkl) {
                for e in entries.flatten() {
                    let inner = e.path().join("include/mkl.h");
                    if inner.is_file() {
                        // Move contents up one level.
                        for f in std::fs::read_dir(e.path())?.flatten() {
                            let dst = mkl.join(f.file_name());
                            if !dst.exists() {
                                std::fs::rename(f.path(), dst)?;
                            }
                        }
                        break;
                    }
                }
            }
        }
        if !mkl_h.is_file() {
            anyhow::bail!("MKL extract did not yield include/mkl.h");
        }
    }

    // 4. Carried patches onto the submodule (idempotent; see native/patches/).
    apply_patch_once(
        &root.join("third_party/bergamot-translator"),
        &root.join("native/patches/msvc-dynamic-crt.patch"),
        &root,
    )?;
    // 5. Configure + build (lib only; app/ CLI never compiles).
    let vcvars = find_vcvars()?;
    let (cmake, cmake_prepend) = cmake_tool()?;
    let mut child_env: Vec<(String, String)> = Vec::new();
    if let Some(d) = cmake_prepend {
        let old = std::env::var("PATH").unwrap_or_default();
        child_env.push(("PATH".to_string(), format!("{d};{old}")));
    }
    let child_env_ref: Vec<(&str, &str)> =
        child_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let toolchain = vcpkg_dir.join("scripts/buildsystems/vcpkg.cmake");
    let build_dir = bn.join("translate");
    let cfg_args: Vec<String> = vec![
        "-S".into(),
        root.join("native").to_string_lossy().into_owned(),
        "-B".into(),
        build_dir.to_string_lossy().into_owned(),
        "-G".into(),
        "Ninja".into(),
        "-DCMAKE_BUILD_TYPE=Release".into(),
        "-DBUILD_ARCH=x86-64".into(),
        "-DCMAKE_POLICY_VERSION_MINIMUM=3.5".into(),
        "-DCOMPILE_TESTS=OFF".into(),
        "-DSSPLIT_USE_INTERNAL_PCRE2=OFF".into(),
        format!("-DCMAKE_TOOLCHAIN_FILE={}", toolchain.display()),
        "-DVCPKG_TARGET_TRIPLET=x64-windows-static".into(),
        format!("-DMKL_DIR={}", mkl.display()),
    ];
    // Fresh configure when the cache is absent; reuses cache otherwise.
    // A failed configure poisons CMakeCache, so start clean in that case.
    if !build_dir.join("build.ninja").is_file() {
        if build_dir.exists() {
            std::fs::remove_dir_all(&build_dir)?;
        }
        run_in_msvc_capture(&vcvars, &cmake, &cfg_args, &root, &child_env_ref)?;
    }
    let mut build_env = child_env_ref.clone();
    build_env.push(("CL", "/utf-8"));
    run_in_msvc(
        &vcvars,
        &cmake,
        &vec![
            "--build".into(),
            build_dir.to_string_lossy().into_owned(),
            "--target".into(),
            "translate_engine".into(),
        ],
        &root,
        &build_env,
    )?;
    println!("[translate] translate_engine built under {}", build_dir.display());
    Ok(())
}
