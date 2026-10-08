// Resident OCR worker: owns the OcrEngine (ort Sessions are !Send-safe only
// when confined) and processes capture jobs in order. The UI thread never
// blocks: it polls the reply channel on its 50ms timer.
use std::sync::mpsc::{channel, Receiver, Sender};

use image::RgbImage;

use crate::{
    config::AppConfig,
    ocr::{models_root, OcrEngine, OcrLine, OcrTimings},
};

pub struct OcrJob {
    pub shot: RgbImage,
}

pub struct OcrReply {
    pub lines: Vec<OcrLine>,
    pub timings: OcrTimings,
    pub tier: String,
    pub error: Option<String>,
}

pub struct OcrWorker {
    tx: Sender<OcrJob>,
    pub rx: Receiver<OcrReply>,
}

impl OcrWorker {
    pub fn spawn(cfg: &AppConfig) -> Self {
        let (job_tx, job_rx) = channel::<OcrJob>();
        let (rep_tx, rep_rx) = channel::<OcrReply>();
        let cascade = cfg.cascade.clone();
        let explicit = cfg.explicit_model.clone();
        let batch = cfg.rec_batch_size;
        let use_dml = cfg.use_directml;
        std::thread::Builder::new()
            .name("ocr-worker".to_string())
            .spawn(move || {
                let root = models_root();
                let engine = OcrEngine::load_cascade(&root, &cascade, explicit.as_deref(), batch, use_dml);
                let mut engine = match engine {
                    Ok((e, _)) => e,
                    Err(e) => {
                        // Poison every future job with the load error.
                        let msg = format!("{e:#}");
                        while job_rx.recv().is_ok() {
                            let _ = rep_tx.send(OcrReply {
                                lines: Vec::new(),
                                timings: OcrTimings::default(),
                                tier: String::new(),
                                error: Some(msg.clone()),
                            });
                        }
                        return;
                    }
                };
                for job in job_rx {
                    match engine.run(&job.shot) {
                        Ok((lines, timings)) => {
                            let _ = rep_tx.send(OcrReply {
                                lines,
                                timings,
                                tier: engine.tier().to_string(),
                                error: None,
                            });
                        }
                        Err(e) => {
                            let _ = rep_tx.send(OcrReply {
                                lines: Vec::new(),
                                timings: OcrTimings::default(),
                                tier: engine.tier().to_string(),
                                error: Some(format!("{e:#}")),
                            });
                        }
                    }
                }
            })
            .expect("spawn ocr worker");
        Self { tx: job_tx, rx: rep_rx }
    }

    pub fn submit(&self, shot: RgbImage) {
        let _ = self.tx.send(OcrJob { shot });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_delivers_reply_for_blank_image() {
        let cfg = AppConfig::load().unwrap_or(AppConfig {
            combos: Vec::new(),
            explicit_model: None,
            cascade: vec!["small".to_string()],
            rec_batch_size: 6,
            use_directml: false,
            search_url: String::new(),
            translate_url: String::new(),
            save_dir: None,
        });
        let w = OcrWorker::spawn(&cfg);
        w.submit(RgbImage::from_pixel(64, 64, image::Rgb([255, 255, 255])));
        let rep = w.rx.recv_timeout(std::time::Duration::from_secs(120)).expect("reply");
        assert!(rep.error.is_none(), "worker error: {:?}", rep.error);
        assert!(rep.lines.is_empty());
    }
}
