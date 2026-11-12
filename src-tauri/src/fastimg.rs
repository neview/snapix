//! 快速位图编码：把 RGBA 帧零压缩打包成 32bpp top-down BMP。
//! 4K 整屏约 33MB，编码耗时仅为一次内存拷贝（远快于 PNG 压缩），
//! 用于 `shot://` 协议给 WebView 提供冻屏帧 / 编辑图 / 贴图。
//!
//! 全不透明图像（冻屏帧、屏幕裁剪）用 40 字节 BITMAPINFOHEADER；
//! 含半透明像素的图像（剪贴板 PNG 等）改用 108 字节 BITMAPV4HEADER +
//! BI_BITFIELDS alpha 掩码，Chromium 会据此保留透明度。

use image::RgbaImage;

const FILE_HEADER_SIZE: usize = 14;
const INFO_HEADER_SIZE: usize = 40;
const V4_HEADER_SIZE: usize = 108;

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
/// LCS_sRGB（'sRGB' 的 FourCC）
const LCS_SRGB: u32 = 0x7352_4742;

pub fn rgba_to_bmp(img: &RgbaImage) -> Vec<u8> {
    let w = img.width() as usize;
    let h = img.height() as usize;
    let data_size = w * h * 4;
    let raw = img.as_raw();
    let has_alpha = raw.chunks_exact(4).any(|p| p[3] != 0xFF);
    let info_size = if has_alpha {
        V4_HEADER_SIZE
    } else {
        INFO_HEADER_SIZE
    };
    let header_size = FILE_HEADER_SIZE + info_size;

    let mut out = Vec::with_capacity(header_size + data_size);

    // BITMAPFILEHEADER
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((header_size + data_size) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(header_size as u32).to_le_bytes());

    // BITMAPINFOHEADER 公共部分：负高度 = top-down，行序与源数据一致，免翻转
    out.extend_from_slice(&(info_size as u32).to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&(if has_alpha { BI_BITFIELDS } else { BI_RGB }).to_le_bytes());
    out.extend_from_slice(&(data_size as u32).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    if has_alpha {
        // BITMAPV4HEADER 扩展：像素按 BGRA 小端存放 → 掩码 R=0x00FF0000 G=0x0000FF00 B=0x000000FF A=0xFF000000
        out.extend_from_slice(&0x00FF_0000u32.to_le_bytes());
        out.extend_from_slice(&0x0000_FF00u32.to_le_bytes());
        out.extend_from_slice(&0x0000_00FFu32.to_le_bytes());
        out.extend_from_slice(&0xFF00_0000u32.to_le_bytes());
        out.extend_from_slice(&LCS_SRGB.to_le_bytes());
        // CIEXYZTRIPLE 端点 + 三个 gamma：sRGB 下忽略
        out.extend_from_slice(&[0u8; 36 + 12]);
    }

    // 像素：RGBA -> BGRA
    let start = out.len();
    out.resize(start + data_size, 0);
    let dst = &mut out[start..];
    for (d, s) in dst.chunks_exact_mut(4).zip(raw.chunks_exact(4)) {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = if has_alpha { s[3] } else { 0xFF };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(buf: &[u8], off: usize) -> u32 {
        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
    }

    #[test]
    fn opaque_image_uses_plain_info_header() {
        let img = RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]));
        let bmp = rgba_to_bmp(&img);
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32_at(&bmp, 10), 54); // 像素数据偏移 = 14 + 40
        assert_eq!(u32_at(&bmp, 14), 40);
        assert_eq!(u32_at(&bmp, 30), BI_RGB);
        assert_eq!(bmp.len(), 54 + 3 * 2 * 4);
        // 首像素 BGRA
        assert_eq!(&bmp[54..58], &[30, 20, 10, 255]);
    }

    #[test]
    fn translucent_image_uses_v4_header_with_alpha_mask() {
        let mut img = RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]));
        img.put_pixel(1, 1, image::Rgba([9, 8, 7, 128]));
        let bmp = rgba_to_bmp(&img);
        assert_eq!(u32_at(&bmp, 10), 122); // 14 + 108
        assert_eq!(u32_at(&bmp, 14), 108);
        assert_eq!(u32_at(&bmp, 30), BI_BITFIELDS);
        assert_eq!(u32_at(&bmp, 54), 0x00FF_0000); // R mask
        assert_eq!(u32_at(&bmp, 66), 0xFF00_0000); // A mask
        assert_eq!(bmp.len(), 122 + 2 * 2 * 4);
        // 最后一个像素保留 alpha
        let last = &bmp[bmp.len() - 4..];
        assert_eq!(last, &[7, 8, 9, 128]);
    }
}
