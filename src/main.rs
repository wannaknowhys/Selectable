mod capture;
mod config;
mod db;
mod geometry;
mod hotkey;
mod ocr;

use anyhow::Result;
use image::Rgb;
use imageproc::{drawing::draw_hollow_polygon_mut, point::Point as IPoint};

use crate::{config::AppConfig, hotkey::Hotkeys, ocr::OcrEngine};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let once = args.iter().any(|a| a == "--once");
    let image_arg = args.iter().position(|a| a == "--image").and_then(|i| args.get(i + 1)).cloned();
    let mut cfg = AppConfig::load()?;
    if std::env::var("SELECTABLE_CPU").is_ok() {
        cfg.use_directml = false;
    }
    let root = ocr::models_root();
    let (mut engine, tier) =
        OcrEngine::load_cascade(&root, &cfg.cascade, cfg.explicit_model.as_deref(), cfg.rec_batch_size, cfg.use_directml)?;
    println!("selectable: OCR ready (tier={tier})");

    if once || image_arg.is_some() {
        return oneshot(&mut engine, image_arg);
    }

    let hk = Hotkeys::register_shift_printscreen()?;
    println!("selectable: press Shift+PrintScreen to capture+OCR (this window shows results)");
    hk.run_loop(|| {
        if let Err(e) = oneshot(&mut engine, None) {
            eprintln!("capture failed: {e:#}");
        }
    });
    Ok(())
}

fn oneshot(engine: &mut OcrEngine, image_arg: Option<String>) -> Result<()> {
    let shot = match image_arg {
        Some(p) => image::open(&p)?.to_rgb8(),
        None => capture::capture_virtual_screen()?,
    };
    println!("captured {}x{}", shot.width(), shot.height());
    let (lines, t) = engine.run(&shot)?;
    println!(
        "timings ms: det pre {:.0} inf {:.0} post {:.0} | rec pre {:.0} inf {:.0} dec {:.0}",
        t.det_pre_ms, t.det_inf_ms, t.det_post_ms, t.rec_pre_ms, t.rec_inf_ms, t.rec_dec_ms
    );
    println!("--- {} lines (tier={}) ---", lines.len(), engine.tier());
    for l in &lines {
        println!("[{:.2}] {}", l.score, l.text);
    }

    // Annotated verification image in temp/ (gitignored).
    let mut vis = shot.clone();
    for l in &lines {
        let pts: Vec<IPoint<f32>> = l
            .quad
            .points
            .iter()
            .map(|p| IPoint::new(p[0], p[1]))
            .collect();
        draw_hollow_polygon_mut(&mut vis, &pts, Rgb([255, 0, 0]));
    }
    std::fs::create_dir_all("temp")?;
    vis.save("temp/shot.png")?;
    println!("annotated image -> temp/shot.png");
    Ok(())
}
