// Full-virtual-screen capture via GDI BitBlt (simple, dependable for M1).
// A DXGI Desktop Duplication path can replace this later for speed/VRR.
use anyhow::{Context, Result};
use image::RgbImage;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HBITMAP, SRCCOPY,
    },
    UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    },
};

pub fn capture_virtual_screen() -> Result<RgbImage> {
    const NULL_HWND: HWND = HWND(std::ptr::null_mut());
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 {
            anyhow::bail!("invalid virtual screen size {w}x{h}");
        }
        let (w, h) = (w as u32, h as u32);

        let src_dc = GetDC(Some(NULL_HWND));
        if src_dc.is_invalid() {
            anyhow::bail!("GetDC failed");
        }
        let mem_dc = CreateCompatibleDC(Some(src_dc));
        if mem_dc.is_invalid() {
            let _ = ReleaseDC(Some(NULL_HWND), src_dc);
            anyhow::bail!("CreateCompatibleDC failed");
        }
        let bmp: HBITMAP = CreateCompatibleBitmap(src_dc, w as i32, h as i32);
        if bmp.is_invalid() {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(Some(NULL_HWND), src_dc);
            anyhow::bail!("CreateCompatibleBitmap failed");
        }
        let old = SelectObject(mem_dc, bmp.into());
        BitBlt(mem_dc, 0, 0, w as i32, h as i32, Some(src_dc), x, y, SRCCOPY)
            .context("BitBlt failed")?;

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0 as u32,
            ..std::mem::zeroed()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(
            mem_dc,
            bmp,
            0,
            h,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        SelectObject(mem_dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(Some(NULL_HWND), src_dc);
        if lines == 0 {
            anyhow::bail!("GetDIBits returned 0 lines");
        }

        // BGRA -> RGB
        let mut rgb = RgbImage::new(w, h);
        for (i, px) in rgb.pixels_mut().enumerate() {
            let o = i * 4;
            px.0 = [buf[o + 2], buf[o + 1], buf[o]];
        }
        Ok(rgb)
    }
}
