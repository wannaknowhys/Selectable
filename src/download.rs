// Runtime file downloader over WinHTTP (zero third-party deps).
// Chunked reads report (done_bytes, total_bytes?) and honor an atomic cancel
// flag; partial files are removed so a retry starts clean.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::Win32::Networking::WinHttp::*;

fn null_handle() -> *mut core::ffi::c_void {
    std::ptr::null_mut()
}

struct Session(*mut core::ffi::c_void);
impl Session {
    fn open() -> anyhow::Result<Self> {
        let h = unsafe {
            WinHttpOpen(
                windows::core::w!("Selectable"),
                WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                windows::core::PCWSTR::null(),
                windows::core::PCWSTR::null(),
                0,
            )
        };
        if h.is_null() {
            anyhow::bail!("WinHttpOpen failed");
        }
        Ok(Self(h))
    }
    fn connect(&self, host: &str) -> anyhow::Result<Conn> {
        let host_w: Vec<u16> = host.encode_utf16().chain([0]).collect();
        let h = unsafe {
            WinHttpConnect(
                self.0,
                windows::core::PCWSTR(host_w.as_ptr()),
                443,
                0,
            )
        };
        if h.is_null() {
            anyhow::bail!("WinHttpConnect({host}) failed");
        }
        Ok(Conn(h))
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

struct Conn(*mut core::ffi::c_void);
impl Drop for Conn {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

fn status_of(req: *mut core::ffi::c_void) -> anyhow::Result<u32> {
    unsafe {
        let mut code: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            req,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            windows::core::PCWSTR::null(),
            Some(&mut code as *mut u32 as *mut core::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(|e| anyhow::anyhow!("status query failed: {e}"))?;
        Ok(code)
    }
}

fn header_text(req: *mut core::ffi::c_void, query: u32) -> Option<String> {
    unsafe {
        let mut len: u32 = 0;
        let _ = WinHttpQueryHeaders(
            req,
            query,
            windows::core::PCWSTR::null(),
            None,
            &mut len,
            std::ptr::null_mut(),
        );
        if len < 2 {
            return None;
        }
        let mut buf = vec![0u16; (len / 2) as usize];
        WinHttpQueryHeaders(
            req,
            query,
            windows::core::PCWSTR::null(),
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .ok()?;
        Some(String::from_utf16_lossy(&buf).trim_end_matches('\0').to_string())
    }
}

fn split_url(url: &str) -> anyhow::Result<(String, String)> {
    let rest = url.strip_prefix("https://").ok_or_else(|| anyhow::anyhow!("only https supported"))?;
    let (host, path) = rest.split_once('/').ok_or_else(|| anyhow::anyhow!("bad url"))?;
    Ok((host.to_string(), format!("/{path}")))
}

/// GET `url` to `dest`, following up to 5 redirects.
/// `progress(done, total?)` is called per chunk; `cancel` aborts promptly.
pub fn download(
    url: &str,
    dest: &Path,
    cancel: &Arc<AtomicBool>,
    mut progress: impl FnMut(u64, Option<u64>),
) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Download to a temp name, rename on success (never leave half files).
    let tmp = dest.with_extension("part");
    let _ = std::fs::remove_file(&tmp);

    let session = Session::open()?;
    let mut url = url.to_string();
    let mut out: Option<std::fs::File> = None;
    let mut done: u64 = 0;
    let mut total: Option<u64> = None;

    for _ in 0..6 {
        if cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!("download cancelled");
        }
        let (host, path) = split_url(&url)?;
        let conn = session.connect(&host)?;
        let path_w: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let req = unsafe {
            WinHttpOpenRequest(
                conn.0,
                windows::core::w!("GET"),
                windows::core::PCWSTR(path_w.as_ptr()),
                windows::core::PCWSTR::null(),
                windows::core::PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        };
        if req.is_null() {
            anyhow::bail!("WinHttpOpenRequest failed");
        }
        struct Req(*mut core::ffi::c_void);
        impl Drop for Req {
            fn drop(&mut self) {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
        let req = Req(req);
        unsafe {
            WinHttpSendRequest(req.0, None, None, 0, 0, 0)
                .map_err(|e| anyhow::anyhow!("send failed: {e}"))?;
            WinHttpReceiveResponse(req.0, std::ptr::null_mut())
                .map_err(|e| anyhow::anyhow!("response failed: {e}"))?;
        }
        match status_of(req.0)? {
            200 => {
                if out.is_none() {
                    // Content length is informational only; sizes verify later.
                    if let Some(cl) = header_text(req.0, WINHTTP_QUERY_CONTENT_LENGTH) {
                        total = cl.parse::<u64>().ok();
                    }
                    out = Some(std::fs::File::create(&tmp)?);
                }
                let f = out.as_mut().unwrap();
                use std::io::Write as _;
                let mut buf = vec![0u8; 65536];
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        drop(out.take());
                        let _ = std::fs::remove_file(&tmp);
                        anyhow::bail!("download cancelled");
                    }
                    let mut got: u32 = 0;
                    unsafe {
                        WinHttpReadData(
                            req.0,
                            buf.as_mut_ptr() as *mut core::ffi::c_void,
                            buf.len() as u32,
                            &mut got,
                        )
                        .map_err(|e| anyhow::anyhow!("read failed: {e}"))?;
                    }
                    if got == 0 {
                        break;
                    }
                    f.write_all(&buf[..got as usize])?;
                    done += got as u64;
                    progress(done, total);
                }
                drop(out.take());
                std::fs::rename(&tmp, dest)?;
                return Ok(());
            }
            301 | 302 | 303 | 307 | 308 => {
                let loc = header_text(req.0, WINHTTP_QUERY_LOCATION)
                    .ok_or_else(|| anyhow::anyhow!("redirect without location"))?;
                // Absolute or path-absolute; resolve against current URL.
                url = if loc.starts_with("https://") {
                    loc
                } else {
                    let (host, _) = split_url(&url)?;
                    format!("https://{host}{loc}")
                };
                continue;
            }
            code => anyhow::bail!("HTTP {code} for {url}"),
        }
    }
    let _ = std::fs::remove_file(&tmp);
    anyhow::bail!("too many redirects")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_small_file_with_progress() {
        let dir = std::env::temp_dir().join("selectable-dl-test");
        let _ = std::fs::remove_dir_all(&dir);
        let dest = dir.join("models.json");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut calls = 0;
        download(
            "https://storage.googleapis.com/moz-fx-translations-data--303e-prod-translations-data/db/models.json",
            &dest,
            &cancel,
            |_, _| calls += 1,
        )
        .expect("download");
        assert!(dest.is_file());
        assert!(calls > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_download_errors_and_cleans_up() {
        let dir = std::env::temp_dir().join("selectable-dl-cancel");
        let _ = std::fs::remove_dir_all(&dir);
        let dest = dir.join("x.bin");
        let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled
        let r = download("https://storage.googleapis.com/", &dest, &cancel, |_, _| {});
        assert!(r.is_err());
        assert!(!dest.exists() && !dest.with_extension("part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
