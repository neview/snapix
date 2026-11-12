//! 截图带光标：抓取系统当前光标位图（含热点 / 屏幕位置），并合成进截图。
//!
//! 取图不依赖 `DrawIconEx`（alpha 行为随驱动而变），而是直接 `GetDIBits` 读
//! 光标的两张位图：
//! - 彩色位图有 alpha → 直接用它的 alpha（Win7 之后带抗锯齿的箭头都走这条）；
//! - alpha 全零（老式光标）→ 退回 1bpp mask：白 = 透明、黑 = 不透明。
//!
//! 合成在冻屏帧抓到的那一刻完成，因此复制 / 保存 / 贴图 / 标注拿到的都是带光标的图。

use image::RgbaImage;

/// 一次光标快照。
pub struct CursorShot {
    /// 光标位图（BGRA → RGBA，尺寸为实际像素）
    pub image: RgbaImage,
    /// 热点在位图内的坐标
    pub hot: (i32, i32),
    /// 热点在虚拟桌面上的物理坐标
    pub pos: (i32, i32),
}

#[cfg(windows)]
pub fn snapshot() -> Option<CursorShot> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, HICON, ICONINFO,
    };

    /// 把一张 HBITMAP 以 32bpp 自顶向下读出。
    unsafe fn dib_32(hdc: HDC, hbm: HBITMAP, w: u32, h: u32) -> Option<Vec<u8>> {
        let bmi_header = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            // 负高度 = 自顶向下，省一次翻转
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut bi = BITMAPINFO {
            bmiHeader: bmi_header,
            ..Default::default()
        };
        let mut buf = vec![0u8; w as usize * h as usize * 4];
        let lines = GetDIBits(
            hdc,
            hbm,
            0,
            h,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );
        (lines == h as i32).then_some(buf)
    }

    unsafe {
        let mut ci = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 {
            return None;
        }
        let mut ii = ICONINFO::default();
        if GetIconInfo(HICON(ci.hCursor.0), &mut ii).is_err() {
            return None;
        }
        // 拿到的两张位图都要自己释放
        struct Bitmaps(HBITMAP, HBITMAP);
        impl Drop for Bitmaps {
            fn drop(&mut self) {
                use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ};
                unsafe {
                    for h in [self.0, self.1] {
                        if !h.is_invalid() {
                            let _ = DeleteObject(HGDIOBJ(h.0));
                        }
                    }
                }
            }
        }
        let _guard = Bitmaps(ii.hbmColor, ii.hbmMask);

        let mut bmp = BITMAP::default();
        let probe = if !ii.hbmColor.is_invalid() {
            ii.hbmColor
        } else {
            ii.hbmMask
        };
        if GetObjectW(
            HGDIOBJ(probe.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bmp as *mut _ as *mut core::ffi::c_void),
        ) == 0
        {
            return None;
        }
        // 没有彩色位图时 mask 是「上下两半」的结构，高度要减半
        let h = if ii.hbmColor.is_invalid() {
            (bmp.bmHeight / 2).max(1)
        } else {
            bmp.bmHeight
        } as u32;
        let w = bmp.bmWidth.max(1) as u32;
        if w > 512 || h > 512 {
            return None;
        }

        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return None;
        }
        let color = if ii.hbmColor.is_invalid() {
            None
        } else {
            dib_32(hdc, ii.hbmColor, w, h)
        };
        let alpha_in_color = color
            .as_ref()
            .is_some_and(|c| c.as_chunks::<4>().0.iter().any(|p| p[3] != 0));
        let mask = if alpha_in_color {
            None
        } else if !ii.hbmMask.is_invalid() {
            dib_32(hdc, ii.hbmMask, w, h)
        } else {
            None
        };
        ReleaseDC(None, hdc);

        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for i in 0..(w as usize * h as usize) {
            // 彩色图无 alpha 时靠 mask 决定透明度（mask 白 = 透）；单色光标直接按 mask 画黑白
            let mask_clear = mask.as_ref().is_some_and(|m| m[i * 4] >= 128);
            let (b, g, r, a) = match &color {
                Some(c) => {
                    let p = &c[i * 4..i * 4 + 4];
                    let a = if alpha_in_color {
                        p[3]
                    } else if mask_clear {
                        0
                    } else {
                        255
                    };
                    (p[0], p[1], p[2], a)
                }
                None if mask_clear => (0, 0, 0, 0),
                None => (255, 255, 255, 255),
            };
            rgba[i * 4] = r;
            rgba[i * 4 + 1] = g;
            rgba[i * 4 + 2] = b;
            rgba[i * 4 + 3] = a;
        }

        let POINT { x, y } = ci.ptScreenPos;
        Some(CursorShot {
            image: RgbaImage::from_raw(w, h, rgba)?,
            hot: (ii.xHotspot as i32, ii.yHotspot as i32),
            pos: (x, y),
        })
    }
}

#[cfg(not(windows))]
pub fn snapshot() -> Option<CursorShot> {
    None
}

/// 把光标按 alpha 混合到 `frame` 上；`origin` 是 `frame` 左上角在虚拟桌面上的物理坐标。
/// 光标大部分落在选区外时只画交集部分（不裁掉整只光标，避免箭头缺一角）。
pub fn composite(frame: &mut RgbaImage, shot: &CursorShot, origin: (i32, i32)) {
    let (fw, fh) = (frame.width() as i32, frame.height() as i32);
    let left = shot.pos.0 - shot.hot.0 - origin.0;
    let top = shot.pos.1 - shot.hot.1 - origin.1;
    let (cw, ch) = (shot.image.width() as i32, shot.image.height() as i32);

    let x0 = left.max(0);
    let y0 = top.max(0);
    let x1 = (left + cw).min(fw);
    let y1 = (top + ch).min(fh);
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let dst = frame.as_mut();
    let src = shot.image.as_raw();
    let fstride = fw as usize * 4;
    let sstride = cw as usize * 4;
    for y in y0..y1 {
        let sy = (y - top) as usize;
        let mut di = y as usize * fstride + x0 as usize * 4;
        let mut si = sy * sstride + (x0 - left) as usize * 4;
        for _ in x0..x1 {
            let a = src[si + 3] as u32;
            if a == 255 {
                dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
            } else if a != 0 {
                let inv = 255 - a;
                for k in 0..3 {
                    dst[di + k] = ((src[si + k] as u32 * a + dst[di + k] as u32 * inv) / 255) as u8;
                }
                dst[di + 3] = 255;
            }
            di += 4;
            si += 4;
        }
    }
}

/// 便捷入口：按设置决定是否取光标，并在帧坐标内合成。
/// `origin` 为帧左上角的虚拟桌面物理坐标。
pub fn apply(frame: &mut RgbaImage, origin: (i32, i32)) {
    if let Some(shot) = snapshot() {
        composite(frame, &shot, origin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(px))
    }

    #[test]
    fn blends_inside() {
        let mut frame = solid(10, 10, [0, 0, 0, 255]);
        let shot = CursorShot {
            image: solid(2, 2, [255, 255, 255, 255]),
            hot: (0, 0),
            pos: (3, 4),
        };
        composite(&mut frame, &shot, (0, 0));
        assert_eq!(frame.get_pixel(3, 4), &Rgba([255, 255, 255, 255]));
        assert_eq!(frame.get_pixel(2, 4), &Rgba([0, 0, 0, 255]));
    }

    #[test]
    fn clips_at_edges() {
        let mut frame = solid(10, 10, [0, 0, 0, 255]);
        let shot = CursorShot {
            image: solid(4, 4, [255, 0, 0, 128]),
            hot: (2, 2),
            pos: (1, 1),
        };
        // 光标左上角落在 (-1,-1)：只画右下 3×3
        composite(&mut frame, &shot, (0, 0));
        assert_eq!(frame.get_pixel(0, 0), &Rgba([128, 0, 0, 255]));
        assert_eq!(frame.get_pixel(9, 9), &Rgba([0, 0, 0, 255]));
    }

    #[test]
    fn skips_fully_transparent() {
        let mut frame = solid(4, 4, [0, 0, 0, 255]);
        let shot = CursorShot {
            image: solid(2, 2, [255, 255, 255, 0]),
            hot: (0, 0),
            pos: (0, 0),
        };
        composite(&mut frame, &shot, (0, 0));
        assert_eq!(frame.get_pixel(0, 0), &Rgba([0, 0, 0, 255]));
    }
}
