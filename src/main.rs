#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod clipboard;
mod config;
mod db;
mod geometry;
mod hotkey;
mod ocr;
mod overlay;
mod tray;
mod worker;

use anyhow::Result;
use image::Rgb;
use imageproc::{drawing::draw_hollow_polygon_mut, point::Point as IPoint};

use crate::{config::AppConfig, ocr::OcrEngine, worker::OcrWorker};

fn main() -> Result<()> {
    unsafe {
        // Physical pixels everywhere so overlay coords match the capture.
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let args: Vec<String> = std::env::args().collect();
    let once = args.iter().any(|a| a == "--once");
    let show_ui = args.iter().any(|a| a == "--overlay");
    let overlay_test = args.iter().any(|a| a == "--overlay-test");
    let image_arg = args.iter().position(|a| a == "--image").and_then(|i| args.get(i + 1)).cloned();
    let mut cfg = AppConfig::load()?;
    if std::env::var("SELECTABLE_CPU").is_ok() {
        cfg.use_directml = false;
    }

    if once || image_arg.is_some() {
        // Synchronous headless path (debug/CI): own engine, no tray.
        let root = ocr::models_root();
        let (mut engine, tier) = OcrEngine::load_cascade(
            &root,
            &cfg.cascade,
            cfg.explicit_model.as_deref(),
            cfg.rec_batch_size,
            cfg.use_directml,
        )?;
        println!("selectable: OCR ready (tier={tier})");
        return oneshot(&mut engine, image_arg);
    }

    // Resident path: worker owns the engine; tray owns the message loop.
    let worker = OcrWorker::spawn(&cfg);
    if show_ui || overlay_test {
        return capture_and_show(&worker, &cfg, overlay_test.then_some(4000));
    }
    tray::run_tray(worker, cfg)
}

pub(crate) fn capture_and_show(
    worker: &OcrWorker,
    cfg: &AppConfig,
    autoclose_ms: Option<u32>,
) -> Result<()> {
    let shot = capture::capture_virtual_screen()?;
    // Foreground window is still the user's app here; the overlay pops after.
    let title = overlay::active_window_title();
    worker.submit(shot.clone());
    let req = overlay::OverlayRequest {
        shot,
        active_title: title,
        search_url: cfg.search_url.clone(),
        translate_url: cfg.translate_url.clone(),
        save_dir: cfg.save_dir.clone(),
    };
    match autoclose_ms {
        Some(ms) => overlay::show_overlay_autoclose(req, &worker.rx, ms),
        None => overlay::show_overlay(req, &worker.rx),
    }
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
