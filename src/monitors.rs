// Monitor enumeration (multi-screen support): rects in physical pixels,
// matching the DXGI capture coordinate space.
use windows::{
    core::BOOL,
    Win32::{
        Foundation::{LPARAM, POINT, RECT, TRUE},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
            MONITOR_DEFAULTTONEAREST,
        },
        UI::WindowsAndMessaging::MONITORINFOF_PRIMARY,
    },
};

#[derive(Debug, Clone, Copy)]
pub struct Mon {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
    pub primary: bool,
}

impl Mon {
    pub fn cx(&self) -> i32 {
        (self.l + self.r) / 2
    }
    pub fn cy(&self) -> i32 {
        (self.t + self.b) / 2
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
}

unsafe extern "system" fn enum_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _rc: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let out = &mut *(data.0 as *mut Vec<Mon>);
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(hmon, &mut mi).as_bool() {
        out.push(Mon {
            l: mi.rcMonitor.left,
            t: mi.rcMonitor.top,
            r: mi.rcMonitor.right,
            b: mi.rcMonitor.bottom,
            primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    TRUE
}

/// All monitors; falls back to the virtual screen if enumeration fails.
pub fn enum_monitors(vx: i32, vy: i32, vw: i32, vh: i32) -> Vec<Mon> {
    let mut out = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_cb), LPARAM(&mut out as *mut Vec<Mon> as isize));
    }
    if out.is_empty() {
        out.push(Mon { l: vx, t: vy, r: vx + vw, b: vy + vh, primary: true });
    }
    out
}

/// Monitor containing the cursor (nearest on a tie) — the user's focus screen.
pub fn focus_monitor(mons: &[Mon], cursor: (i32, i32)) -> Mon {
    unsafe {
        let h = MonitorFromPoint(
            POINT { x: cursor.0, y: cursor.1 },
            MONITOR_DEFAULTTONEAREST,
        );
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if !h.is_invalid() && GetMonitorInfoW(h, &mut mi).as_bool() {
            let m = Mon {
                l: mi.rcMonitor.left,
                t: mi.rcMonitor.top,
                r: mi.rcMonitor.right,
                b: mi.rcMonitor.bottom,
                primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0,
            };
            return m;
        }
    }
    mons
        .iter()
        .find(|m| m.contains(cursor.0, cursor.1))
        .copied()
        .or_else(|| mons.iter().find(|m| m.primary).copied())
        .unwrap_or(mons[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitors_enumerate_with_sane_rects() {
        let mons = enum_monitors(0, 0, 1920, 1080);
        assert!(!mons.is_empty());
        for m in &mons {
            assert!(m.r > m.l && m.b > m.t, "bad rect {m:?}");
        }
    }
}
