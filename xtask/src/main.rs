// cargo xtask dist [--debug] [--tier small|medium|all]
// Assembles dist/Selectable/ (gitignored green layout):
//   Selectable.exe + DirectML.dll + models/<tier>/... + config.toml (from example if missing)
// Missing model tiers present in the repo lock are backfilled via tools/fetch-models.mjs.
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    // xtask lives at <root>/xtask; CARGO_MANIFEST_DIR is compile-time reliable.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn run(cmd: &str, args: &[&str], dir: &Path) -> anyhow::Result<()> {
    let st = std::process::Command::new(cmd).args(args).current_dir(dir).status()?;
    if !st.success() {
        anyhow::bail!("command failed: {cmd} {}", args.join(" "));
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::copy(src, dst)?;
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> anyhow::Result<u64> {
    let mut bytes = 0u64;
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let (s, d) = (e.path(), dst.join(e.file_name()));
        if e.file_type()?.is_dir() {
            bytes += copy_dir(&s, &d)?;
        } else {
            bytes += std::fs::copy(&s, &d)?;
        }
    }
    Ok(bytes)
}

fn triple_complete(dir: &Path) -> bool {
    ["det.onnx", "det.yml", "rec.onnx", "rec.yml"].iter().all(|f| dir.join(f).is_file())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) != Some("dist") {
        eprintln!("usage: cargo xtask dist [--debug] [--tier small|medium|all]");
        std::process::exit(2);
    }
    let debug = args.iter().any(|a| a == "--debug");
    let tier = args.iter().position(|a| a == "--tier").and_then(|i| args.get(i + 1)).cloned();

    let root = root();
    let profile = if debug { "debug" } else { "release" };

    // 1. Build the binary.
    println!("[xtask] cargo build --profile {profile}");
    if debug {
        run("cargo", &["build"], &root)?;
    } else {
        run("cargo", &["build", "--release"], &root)?;
    }

    // 2. Backfill models into the repo (dist copies from here).
    let tiers: Vec<String> = match tier.as_deref() {
        Some("small") | Some("medium") => vec![tier.unwrap()],
        _ => vec!["small".to_string(), "medium".to_string()],
    };
    for t in &tiers {
        if !triple_complete(&root.join("models").join(t)) {
            println!("[xtask] models/{t} incomplete -> fetching");
            run("node", &["tools/fetch-models.mjs", "--tier", t], &root)?;
        }
    }

    // 3. Assemble dist/Selectable.
    let dist = root.join("dist").join("Selectable");
    if dist.exists() {
        println!("[xtask] reusing existing dist (user config.toml is never overwritten)");
    }
    std::fs::create_dir_all(&dist)?;
    let exe_src = root.join("target").join(profile).join(if cfg!(windows) { "selectable.exe" } else { "selectable" });
    copy_file(&exe_src, &dist.join(if cfg!(windows) { "Selectable.exe" } else { "selectable" }))?;
    println!("[xtask] exe <- {}", exe_src.display());

    // Sibling runtime DLLs (DirectML etc.) that ort drops next to the binary.
    let target_dir = root.join("target").join(profile);
    for e in std::fs::read_dir(&target_dir)? {
        let e = e?;
        if e.path().extension().map(|x| x == "dll").unwrap_or(false) {
            copy_file(&e.path(), &dist.join(e.file_name()))?;
            println!("[xtask] dll <- {}", e.file_name().to_string_lossy());
        }
    }

    let mut total = 0u64;
    for t in &tiers {
        let src = root.join("models").join(t);
        if triple_complete(&src) {
            total += copy_dir(&src, &dist.join("models").join(t))?;
            println!("[xtask] models/{t} copied");
        } else {
            println!("[xtask] models/{t} still incomplete, skipped");
        }
    }

    let cfg = dist.join("config.toml");
    if !cfg.exists() {
        copy_file(&root.join("config.example.toml"), &cfg)?;
        println!("[xtask] config.toml seeded from example");
    }

    println!(
        "[xtask] done: {} ({:.1} MB models)",
        dist.display(),
        total as f64 / 1048576.0
    );
    Ok(())
}
