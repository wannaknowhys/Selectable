//! About dialog: version + third-party attribution (DESIGN: same names as
//! THIRD-PARTY-NOTICES, including the Mozilla / Bergamot EU-grant line).
//! Text building is pure (unit-tested); display is one MessageBoxW call.
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK},
    },
};

/// Dialog body. Version comes from Cargo.toml (single source of truth).
pub fn about_text() -> String {
    format!(
        "Selectable {}\n\
         Windows 截屏即选词（离线优先，绿色免安装）\n\
         \n\
         OCR：PaddlePaddle PP-OCRv6（Apache-2.0）\n\
         翻译：Bergamot / Marian（MPL-2.0 / MIT）+ Firefox 翻译模型（MPL-2.0）\n\
         Attribution: Mozilla / Bergamot consortium, EU Horizon 2020 grant 825303\n\
         \n\
         完整第三方声明见程序目录 THIRD-PARTY-NOTICES。",
        env!("CARGO_PKG_VERSION")
    )
}

pub fn show_about(hwnd: HWND) {
    let text: Vec<u16> = about_text().encode_utf16().chain([0]).collect();
    // Null hwnd (tray has no visible window) => no owner, same dialog.
    let owner = if hwnd.0.is_null() { None } else { Some(hwnd) };
    unsafe {
        MessageBoxW(owner, PCWSTR(text.as_ptr()), w!("关于 Selectable"), MB_OK | MB_ICONINFORMATION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_names_version_and_grant() {
        let t = about_text();
        assert!(t.contains(env!("CARGO_PKG_VERSION")), "version missing");
        assert!(t.contains("825303"), "EU grant missing");
        assert!(t.contains("THIRD-PARTY-NOTICES"), "notices pointer missing");
        assert!(t.contains("Mozilla"), "attribution missing");
    }
}
