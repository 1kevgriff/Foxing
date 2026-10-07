//! GDI renderer for [`DrawList`]s and [`Measure`] implementation for the UI layer.

use foxing::ui::{Cmd, Color, DrawList, Font, Measure};
use std::mem::zeroed;
use std::ptr::{null, null_mut};
use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;

/// Fonts plus a scratch DC for measuring. Fonts are owned by whoever created them.
pub struct Gdi {
    dc: HDC,
    text_font: HFONT,
    ui_font: HFONT,
    dpi: u32,
    heights: [i32; 2],
}

impl Gdi {
    pub unsafe fn new(text_font: HFONT, ui_font: HFONT, dpi: u32) -> Self {
        let dc = CreateCompatibleDC(null_mut());
        let mut g = Gdi {
            dc,
            text_font,
            ui_font,
            dpi,
            heights: [0; 2],
        };
        for (i, f) in [Font::Text, Font::Ui].into_iter().enumerate() {
            SelectObject(dc, g.font(f) as HGDIOBJ);
            let mut tm: TEXTMETRICW = zeroed();
            GetTextMetricsW(dc, &mut tm);
            g.heights[i] = tm.tmHeight + tm.tmExternalLeading;
        }
        g
    }

    pub fn font(&self, f: Font) -> HFONT {
        match f {
            Font::Text => self.text_font,
            Font::Ui => self.ui_font,
        }
    }
}

impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe { DeleteDC(self.dc) };
    }
}

impl Measure for Gdi {
    fn text_width(&self, font: Font, s: &str) -> i32 {
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        unsafe {
            SelectObject(self.dc, self.font(font) as HGDIOBJ);
            GetTextExtentPoint32W(self.dc, w.as_ptr(), w.len() as i32, &mut size);
        }
        size.cx
    }

    fn line_height(&self, font: Font) -> i32 {
        self.heights[font as usize]
    }

    fn dpi(&self) -> u32 {
        self.dpi
    }
}

/// 0xRRGGBB to a GDI COLORREF (0x00BBGGRR).
pub fn colorref(c: Color) -> COLORREF {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

unsafe fn fill(hdc: HDC, rc: &RECT, color: Color) {
    let brush = CreateSolidBrush(colorref(color));
    FillRect(hdc, rc, brush);
    DeleteObject(brush as HGDIOBJ);
}

pub unsafe fn render(hdc: HDC, dl: &DrawList, g: &Gdi) {
    SetBkMode(hdc, TRANSPARENT as i32);
    let mut units: Vec<u16> = Vec::new();
    let mut dx: Vec<i32> = Vec::new();
    for cmd in &dl.cmds {
        match cmd {
            Cmd::Fill { rect, color } => {
                let rc = RECT {
                    left: rect.x,
                    top: rect.y,
                    right: rect.right(),
                    bottom: rect.bottom(),
                };
                fill(hdc, &rc, *color);
            }
            Cmd::Line { from, to, color } => {
                let rc = RECT {
                    left: from.0.min(to.0),
                    top: from.1.min(to.1),
                    right: from.0.max(to.0) + 1,
                    bottom: from.1.max(to.1) + 1,
                };
                fill(hdc, &rc, *color);
            }
            Cmd::Text {
                x,
                y,
                font,
                color,
                text,
                clip,
            } => {
                units.clear();
                units.extend(text.encode_utf16());
                let rc = RECT {
                    left: clip.x,
                    top: clip.y,
                    right: clip.right(),
                    bottom: clip.bottom(),
                };
                SelectObject(hdc, g.font(*font) as HGDIOBJ);
                SetTextColor(hdc, colorref(*color));
                ExtTextOutW(
                    hdc,
                    *x,
                    *y,
                    ETO_CLIPPED,
                    &rc,
                    units.as_ptr(),
                    units.len() as u32,
                    null(),
                );
            }
            Cmd::Glyphs {
                x,
                y,
                color,
                bg,
                chars,
                advances,
            } => {
                units.clear();
                dx.clear();
                let mut b = [0u16; 2];
                for (c, a) in chars.iter().zip(advances) {
                    let enc = c.encode_utf16(&mut b);
                    units.extend_from_slice(enc);
                    dx.push(*a);
                    if enc.len() == 2 {
                        dx.push(0);
                    }
                }
                SelectObject(hdc, g.font(Font::Text) as HGDIOBJ);
                SetTextColor(hdc, colorref(*color));
                match bg {
                    Some((bg, h)) => {
                        let rc = RECT {
                            left: *x,
                            top: *y,
                            right: x + advances.iter().sum::<i32>(),
                            bottom: y + h,
                        };
                        SetBkColor(hdc, colorref(*bg));
                        let len = units.len() as u32;
                        ExtTextOutW(
                            hdc,
                            *x,
                            *y,
                            ETO_OPAQUE,
                            &rc,
                            units.as_ptr(),
                            len,
                            dx.as_ptr(),
                        );
                    }
                    None => {
                        let len = units.len() as u32;
                        ExtTextOutW(hdc, *x, *y, 0, null(), units.as_ptr(), len, dx.as_ptr());
                    }
                }
            }
        }
    }
}

/// Paints `hwnd` through an off-screen bitmap to avoid flicker.
pub unsafe fn paint_buffered(hwnd: HWND, f: impl FnOnce(HDC, i32, i32)) {
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = zeroed();
    windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right.max(1), rc.bottom.max(1));
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old = SelectObject(mem, bmp as HGDIOBJ);
    f(mem, w, h);
    BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    DeleteObject(bmp as HGDIOBJ);
    DeleteDC(mem);
    EndPaint(hwnd, &ps);
}

/// Proportional UI font (Segoe UI 9 pt) at `dpi`.
pub unsafe fn ui_font(dpi: u32) -> HFONT {
    CreateFontW(
        -((9 * dpi as i32 + 36) / 72),
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (VARIABLE_PITCH | FF_SWISS) as u32,
        w!("Segoe UI"),
    )
}
