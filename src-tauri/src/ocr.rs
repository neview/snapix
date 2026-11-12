//! 离线 OCR：在 Windows 上使用系统内置的 Windows.Media.Ocr 引擎，
//! 原生支持中文（需系统安装对应 OCR 语言包，中文系统通常已自带 zh-Hans）。
//!
//! 引擎本身没有精度参数，准确率全靠「怎么喂图」：
//! - 自适应缩放：先探测一遍量出字高，再把图缩放到字高约 40px 重新识别。
//!   屏幕上 12~16px 的小字直接送进引擎漏字错字很多，放大后明显改善
//!   （PowerToys 文本提取器同一思路）；
//! - 深底反色：引擎对「浅底深字」最准，深色主题截图先反色再识别；
//! - 四周留白：紧贴选区边缘的文字容易被截掉笔画，识别前补一圈背景色；
//! - 中文去空格：引擎把每个汉字当作独立「词」返回，按字符类型与词框间距决定是否补空格。
//!
//! 额外提供词级包围盒识别与隐私内容（手机号/邮箱/身份证/卡号）定位。

use image::RgbaImage;

use crate::capture::SelRect;

/// 一个识别出的词及其在图像中的包围盒（像素）。
#[derive(Clone, Debug)]
pub struct OcrWord {
    pub text: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// 引擎的理想字高（像素）：过小漏字错字，过大反而下降。
#[cfg(windows)]
const IDEAL_TEXT_HEIGHT: f32 = 40.0;
/// 送入引擎前四周补的留白（缩放后像素）。
#[cfg(windows)]
const PAD: u32 = 32;

#[cfg(windows)]
fn to_bitmap(img: &RgbaImage) -> Result<windows::Graphics::Imaging::SoftwareBitmap, String> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Storage::Streams::DataWriter;

    let (w, h) = (img.width() as i32, img.height() as i32);

    // RGBA -> BGRA8（SoftwareBitmap 需要 BGRA）
    let mut bgra = Vec::with_capacity(img.as_raw().len());
    for px in img.pixels() {
        bgra.push(px[2]);
        bgra.push(px[1]);
        bgra.push(px[0]);
        bgra.push(px[3]);
    }

    let writer = DataWriter::new().map_err(|e| e.to_string())?;
    writer.WriteBytes(&bgra).map_err(|e| e.to_string())?;
    let buffer = writer.DetachBuffer().map_err(|e| e.to_string())?;

    SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w, h)
        .map_err(|e| e.to_string())
}

/// 识别纯文本。走与词级识别相同的缩放 / 分片路径：Windows OCR 引擎有
/// `MaxImageDimension` 上限（多数机器为 2600px），4K 屏上的大选区直接送入会报错。
pub fn recognize(img: &RgbaImage) -> Result<String, String> {
    if img.width() == 0 || img.height() == 0 {
        return Err("空选区".into());
    }
    let lines = recognize_lines(img)?;
    Ok(lines_to_text(&lines))
}

/// 行内词拼接、行以换行连接。
///
/// 引擎把每个汉字当作一个「词」返回，机械地用空格连接会得到「这 是 中 文」。
/// 规则：汉字（含中文标点）之间不补空格；英文 / 数字词之间照常补空格；
/// 中英文交界处看两个词框之间的间距是否够一个空格宽，还原原文有没有空格。
pub fn lines_to_text(lines: &[Vec<OcrWord>]) -> String {
    let mut out = String::new();
    for (li, line) in lines.iter().enumerate() {
        if li > 0 {
            out.push('\n');
        }
        let mut prev: Option<&OcrWord> = None;
        for w in line.iter().filter(|w| !w.text.is_empty()) {
            if let Some(p) = prev {
                if needs_space(p, w) {
                    out.push(' ');
                }
            }
            out.push_str(&w.text);
            prev = Some(w);
        }
    }
    out
}

/// 相邻两个词之间是否应补空格。
fn needs_space(a: &OcrWord, b: &OcrWord) -> bool {
    let (Some(last), Some(first)) = (a.text.chars().last(), b.text.chars().next()) else {
        return false;
    };
    if is_cjk_punct(last) || is_cjk_punct(first) {
        return false;
    }
    match (is_cjk(last), is_cjk(first)) {
        (false, false) => true,
        (true, true) => false,
        // 中英文交界：词框间距达到约 0.2 倍字高才视为原文有空格
        _ => {
            let gap = b.x as i64 - (a.x + a.w) as i64;
            let line_h = a.h.max(b.h) as i64;
            line_h > 0 && gap * 5 >= line_h
        }
    }
}

/// 汉字 / 假名 / 注音及全角字母数字（无词间空格的书写系统；韩文用空格分词，不算在内）。
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x2E80..=0x2FDF   // 康熙 / CJK 部首
        | 0x3040..=0x30FF // 平假名 / 片假名
        | 0x3100..=0x312F // 注音符号
        | 0x31A0..=0x31FF // 注音扩展 / 片假名语音扩展
        | 0x3400..=0x4DBF // CJK 扩展 A
        | 0x4E00..=0x9FFF // CJK 统一表意文字
        | 0xF900..=0xFAFF // CJK 兼容表意文字
        | 0xFF10..=0xFF19 // 全角数字
        | 0xFF21..=0xFF3A // 全角大写字母
        | 0xFF41..=0xFF5A // 全角小写字母
        | 0xFF66..=0xFF9F // 半角片假名
        | 0x20000..=0x2FA1F // CJK 扩展 B 及以后
    )
}

/// 中文标点（前后都不补空格）。不含「“”—…」等中英文共用的符号，以免误伤英文排版。
fn is_cjk_punct(c: char) -> bool {
    matches!(
        c as u32,
        0x3000..=0x303F   // 、。〈〉《》「」【】等
        | 0xFF01..=0xFF0F // ！＂＃＄％＆＇（）＊＋，－．／
        | 0xFF1A..=0xFF20 // ：；＜＝＞？＠
        | 0xFF3B..=0xFF40 // ［＼］＾＿｀
        | 0xFF5B..=0xFF65 // ｛｜｝～｟｠｡｢｣､･
        | 0xFE10..=0xFE1F // 竖排标点
        | 0xFE30..=0xFE4F // CJK 兼容标点
    )
}

/// 词级识别：返回逐行的词与包围盒（原图像素坐标）。
///
/// 流程：深底反色 → 探测字高 → 按理想字高整体缩放 → 超出引擎尺寸上限时纵向分片
/// （滚动长图）→ 每片补留白后识别 → 词框映射回原图坐标。
#[cfg(windows)]
pub fn recognize_lines(img: &RgbaImage) -> Result<Vec<Vec<OcrWord>>, String> {
    use std::borrow::Cow;
    use windows::Media::Ocr::OcrEngine;

    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("空图像".into());
    }

    let engine = create_engine()?;
    // 引擎单边尺寸上限（多数系统 2600px），扣掉两侧留白后才是图像可用尺寸
    let max_dim = OcrEngine::MaxImageDimension()
        .unwrap_or(2600)
        .clamp(256, 8_000);
    let usable = (max_dim - 2 * PAD) as f32;

    // 深底浅字 → 反色（引擎对浅底深字最准）；留白色取反色后的背景色
    let mut bg = border_mode_color(img);
    let src: Cow<RgbaImage> = if luma(bg) < 128.0 {
        bg = [255 - bg[0], 255 - bg[1], 255 - bg[2]];
        Cow::Owned(inverted(img))
    } else {
        Cow::Borrowed(img)
    };
    let ctx = TileCtx {
        engine: &engine,
        bg,
    };

    // 第一遍：以能塞进引擎的最大缩放（通常 1:1）在首片上探测字高
    let probe_scale = (usable / w as f32).min(1.0);
    let probe_h = ((usable / probe_scale) as u32).clamp(1, h);
    let probe_img = image::imageops::crop_imm(&*src, 0, 0, w, probe_h).to_image();
    let probe_lines = ctx.recognize_tile(&probe_img, probe_scale)?;
    let text_h = median_word_height(&probe_lines);

    // 目标缩放：字高 → 约 40px；一个字都没探到则按「字太小」假设放大 2 倍；宽度必须塞进引擎
    let wanted = match text_h {
        Some(th) => (IDEAL_TEXT_HEIGHT / th).clamp(0.5, 4.0),
        None => 2.0,
    };
    let scale = wanted.min(usable / w as f32);

    // 探测已覆盖整图且缩放接近 → 直接采用，省一遍识别
    if probe_h == h && (scale / probe_scale - 1.0).abs() < 0.2 {
        return Ok(probe_lines);
    }

    // 纵向分片：片高按缩放后塞进引擎计算；片间重叠留足几行字，
    // 重叠区以中线划分归属，同一行不会被相邻两片重复收录，也不会被切断
    let tile_h = ((usable / scale) as u32).clamp(1, h);
    let overlap = if tile_h >= h {
        0
    } else {
        ((text_h.unwrap_or(20.0) * 3.0) as u32)
            .max(64)
            .min((tile_h / 3).max(1))
    };
    let half = overlap as f32 / 2.0;

    let mut out: Vec<Vec<OcrWord>> = Vec::new();
    let mut top = 0u32;
    loop {
        let th = tile_h.min(h - top);
        let last = top + th >= h;
        let tile = image::imageops::crop_imm(&*src, 0, top, w, th).to_image();
        for mut line in ctx.recognize_tile(&tile, scale)? {
            for wd in &mut line {
                wd.y += top;
            }
            let center = line_center_y(&line);
            if top > 0 && center < top as f32 + half {
                continue;
            }
            if !last && center >= (top + th) as f32 - half {
                continue;
            }
            out.push(line);
        }
        if last {
            break;
        }
        top += th - overlap;
    }
    Ok(out)
}

/// 创建识别引擎。优先级：用户配置语言若已是中文则直接用；否则优先已安装的中文语言包
/// （中文引擎可同时识别中英文，英文引擎完全不识别汉字）；再退回用户配置语言 / 任一可用语言。
#[cfg(windows)]
fn create_engine() -> Result<windows::Media::Ocr::OcrEngine, String> {
    use windows::Globalization::Language;
    use windows::Media::Ocr::OcrEngine;

    let tag_of = |l: &Language| {
        l.LanguageTag()
            .map(|t| t.to_string().to_ascii_lowercase())
            .unwrap_or_default()
    };

    let profile = OcrEngine::TryCreateFromUserProfileLanguages().ok();
    if let Some(e) = &profile {
        let is_zh = e
            .RecognizerLanguage()
            .map(|l| tag_of(&l).starts_with("zh"))
            .unwrap_or(false);
        if is_zh {
            return Ok(e.clone());
        }
    }

    let available: Vec<Language> = OcrEngine::AvailableRecognizerLanguages()
        .map(|v| v.into_iter().collect())
        .unwrap_or_default();
    let zh = available
        .iter()
        .find(|l| tag_of(l).starts_with("zh-hans"))
        .or_else(|| available.iter().find(|l| tag_of(l).starts_with("zh")));
    if let Some(e) = zh.and_then(|l| OcrEngine::TryCreateFromLanguage(l).ok()) {
        return Ok(e);
    }
    if let Some(e) = profile {
        return Ok(e);
    }
    available
        .iter()
        .find_map(|l| OcrEngine::TryCreateFromLanguage(l).ok())
        .ok_or_else(|| "系统未安装可用的 OCR 语言包".to_string())
}

#[cfg(windows)]
struct TileCtx<'a> {
    engine: &'a windows::Media::Ocr::OcrEngine,
    /// 留白填充色（已按反色处理）
    bg: [u8; 3],
}

#[cfg(windows)]
impl TileCtx<'_> {
    /// 一片图像：缩放 → 四周留白 → 识别 → 词框映射回该片的原始像素坐标。
    fn recognize_tile(&self, tile: &RgbaImage, scale: f32) -> Result<Vec<Vec<OcrWord>>, String> {
        use image::imageops::FilterType;
        use std::borrow::Cow;

        let (tw, th) = tile.dimensions();
        let sw = ((tw as f32 * scale).round() as u32).max(1);
        let sh = ((th as f32 * scale).round() as u32).max(1);
        let scaled: Cow<RgbaImage> = if sw == tw && sh == th {
            Cow::Borrowed(tile)
        } else {
            // 放大用 CatmullRom 保持笔画锐利；缩小用 Triangle 避免锯齿
            let filter = if scale > 1.0 {
                FilterType::CatmullRom
            } else {
                FilterType::Triangle
            };
            Cow::Owned(image::imageops::resize(tile, sw, sh, filter))
        };

        let [r, g, b] = self.bg;
        let mut canvas =
            RgbaImage::from_pixel(sw + 2 * PAD, sh + 2 * PAD, image::Rgba([r, g, b, 255]));
        image::imageops::replace(&mut canvas, &*scaled, PAD as i64, PAD as i64);

        let inv = 1.0 / scale;
        let pad = PAD as f32;
        Ok(run_engine(self.engine, &canvas)?
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .map(|(text, [x, y, w, h])| OcrWord {
                        text,
                        x: ((x - pad) * inv).max(0.0).round() as u32,
                        y: ((y - pad) * inv).max(0.0).round() as u32,
                        w: (w * inv).max(0.0).round() as u32,
                        h: (h * inv).max(0.0).round() as u32,
                    })
                    .collect()
            })
            .collect())
    }
}

/// 引擎原始输出：逐行的（词文本，[x, y, w, h]），坐标为送入图像的像素。
#[cfg(windows)]
type RawLines = Vec<Vec<(String, [f32; 4])>>;

/// 调用引擎，返回原始词与词框。
#[cfg(windows)]
fn run_engine(
    engine: &windows::Media::Ocr::OcrEngine,
    img: &RgbaImage,
) -> Result<RawLines, String> {
    let bitmap = to_bitmap(img)?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    for line in result.Lines().map_err(|e| e.to_string())? {
        let mut words = Vec::new();
        for word in line.Words().map_err(|e| e.to_string())? {
            let text = word.Text().map_err(|e| e.to_string())?.to_string();
            if text.trim().is_empty() {
                continue;
            }
            let r = word.BoundingRect().map_err(|e| e.to_string())?;
            words.push((text, [r.X, r.Y, r.Width, r.Height]));
        }
        if !words.is_empty() {
            out.push(words);
        }
    }
    Ok(out)
}

/// 图像四周 1px 边环上最常见的颜色，近似背景色。
#[cfg(windows)]
fn border_mode_color(img: &RgbaImage) -> [u8; 3] {
    use std::collections::HashMap;

    let (w, h) = img.dimensions();
    let mut counts: HashMap<[u8; 3], u32> = HashMap::new();
    let mut bump = |x: u32, y: u32| {
        let p = img.get_pixel(x, y);
        *counts.entry([p[0], p[1], p[2]]).or_insert(0) += 1;
    };
    for x in 0..w {
        bump(x, 0);
        bump(x, h - 1);
    }
    for y in 1..h.saturating_sub(1) {
        bump(0, y);
        bump(w - 1, y);
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(c, _)| c)
        .unwrap_or([255, 255, 255])
}

#[cfg(windows)]
fn luma(c: [u8; 3]) -> f32 {
    0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32
}

/// 颜色反相（保留 alpha）。
#[cfg(windows)]
fn inverted(img: &RgbaImage) -> RgbaImage {
    let mut out = img.clone();
    for p in out.pixels_mut() {
        p[0] = 255 - p[0];
        p[1] = 255 - p[1];
        p[2] = 255 - p[2];
    }
    out
}

/// 词框高度中位数（比均值更抗标题 / 大字干扰）。
#[cfg(windows)]
fn median_word_height(lines: &[Vec<OcrWord>]) -> Option<f32> {
    let mut hs: Vec<u32> = lines
        .iter()
        .flatten()
        .map(|w| w.h)
        .filter(|h| *h > 0)
        .collect();
    if hs.is_empty() {
        return None;
    }
    hs.sort_unstable();
    Some(hs[hs.len() / 2] as f32)
}

/// 一行词框的垂直中心。
#[cfg(windows)]
fn line_center_y(line: &[OcrWord]) -> f32 {
    let top = line.iter().map(|w| w.y).min().unwrap_or(0);
    let bottom = line.iter().map(|w| w.y + w.h).max().unwrap_or(0);
    (top + bottom) as f32 / 2.0
}

#[cfg(not(windows))]
pub fn recognize_lines(_img: &RgbaImage) -> Result<Vec<Vec<OcrWord>>, String> {
    Err("当前平台暂不支持内置 OCR".into())
}

// ===================== 隐私内容定位 =====================

/// 在 OCR 行上定位隐私内容（手机号 / 邮箱 / 身份证 / 银行卡号），
/// 返回需要打码的图像区域。
pub fn privacy_rects(lines: &[Vec<OcrWord>]) -> Vec<SelRect> {
    let mut rects: Vec<SelRect> = Vec::new();

    for line in lines {
        // 行内词拼接（去空白），记录每个字符归属的词
        let mut concat = String::new();
        let mut owner: Vec<usize> = Vec::new();
        for (i, w) in line.iter().enumerate() {
            for c in w.text.chars().filter(|c| !c.is_whitespace()) {
                concat.push(c);
                owner.push(i);
            }
        }
        if concat.is_empty() {
            continue;
        }

        for (cs, ce) in find_sensitive(&concat) {
            let wi0 = owner[cs];
            let wi1 = owner[ce - 1];
            let hit = &line[wi0..=wi1];
            let x0 = hit.iter().map(|w| w.x).min().unwrap_or(0);
            let y0 = hit.iter().map(|w| w.y).min().unwrap_or(0);
            let x1 = hit.iter().map(|w| w.x + w.w).max().unwrap_or(0);
            let y1 = hit.iter().map(|w| w.y + w.h).max().unwrap_or(0);
            rects.push(SelRect {
                x: x0 as i32,
                y: y0 as i32,
                w: x1 - x0,
                h: y1 - y0,
            });
        }
    }

    merge_rects(rects)
}

/// 在文本中查找隐私片段，返回字符索引区间 [start, end)（已合并重叠）。
pub fn find_sensitive(s: &str) -> Vec<(usize, usize)> {
    use regex::Regex;
    use std::sync::OnceLock;

    struct Pat {
        re: Regex,
        digit_boundary: bool,
    }
    static PATS: OnceLock<Vec<Pat>> = OnceLock::new();
    let pats = PATS.get_or_init(|| {
        vec![
            Pat {
                re: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap(),
                digit_boundary: false,
            },
            Pat {
                re: Regex::new(r"\d{17}[0-9Xx]").unwrap(),
                digit_boundary: true,
            },
            Pat {
                re: Regex::new(r"\d{14,19}").unwrap(),
                digit_boundary: true,
            },
            Pat {
                re: Regex::new(r"1[3-9]\d{9}").unwrap(),
                digit_boundary: true,
            },
        ]
    });

    // 字节偏移 → 字符索引
    let byte_to_char: std::collections::HashMap<usize, usize> = s
        .char_indices()
        .enumerate()
        .map(|(ci, (bi, _))| (bi, ci))
        .collect();
    let total_chars = s.chars().count();
    let chars: Vec<char> = s.chars().collect();

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for pat in pats {
        for m in pat.re.find_iter(s) {
            let cs = byte_to_char[&m.start()];
            let ce = if m.end() == s.len() {
                total_chars
            } else {
                byte_to_char[&m.end()]
            };

            if pat.digit_boundary {
                // 前后紧邻数字则视为更长号码的一部分，跳过
                if cs > 0 && chars[cs - 1].is_ascii_digit() {
                    continue;
                }
                if ce < total_chars && chars[ce].is_ascii_digit() {
                    continue;
                }
            }
            ranges.push((cs, ce));
        }
    }

    // 合并重叠区间
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.0 <= last.1 => last.1 = last.1.max(r.1),
            _ => merged.push(r),
        }
    }
    merged
}

/// 合并相交的矩形（打码块去重）。
fn merge_rects(mut rects: Vec<SelRect>) -> Vec<SelRect> {
    let overlaps = |a: &SelRect, b: &SelRect| {
        a.x < b.x + b.w as i32
            && b.x < a.x + a.w as i32
            && a.y < b.y + b.h as i32
            && b.y < a.y + a.h as i32
    };
    let mut out: Vec<SelRect> = Vec::new();
    while let Some(mut cur) = rects.pop() {
        let mut changed = true;
        while changed {
            changed = false;
            let mut i = 0;
            while i < rects.len() {
                if overlaps(&cur, &rects[i]) {
                    let o = rects.swap_remove(i);
                    let x0 = cur.x.min(o.x);
                    let y0 = cur.y.min(o.y);
                    let x1 = (cur.x + cur.w as i32).max(o.x + o.w as i32);
                    let y1 = (cur.y + cur.h as i32).max(o.y + o.h as i32);
                    cur = SelRect {
                        x: x0,
                        y: y0,
                        w: (x1 - x0) as u32,
                        h: (y1 - y0) as u32,
                    };
                    changed = true;
                } else {
                    i += 1;
                }
            }
        }
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, x: u32) -> OcrWord {
        OcrWord {
            text: text.into(),
            x,
            y: 100,
            w: text.chars().count() as u32 * 10,
            h: 20,
        }
    }

    #[test]
    fn finds_phone_email_idcard() {
        let hits = find_sensitive("联系13812345678或foo.bar@qq.com");
        assert_eq!(hits.len(), 2);
        let hits = find_sensitive("身份证110101199003074518就这样");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0], (3, 21));
    }

    #[test]
    fn digit_boundary_rejects_embedded() {
        // 12 位数字：内嵌手机号段应被边界规则拒绝，也不足 14 位长号
        assert!(find_sensitive("913812345678").is_empty());
        // 恰好 14 位命中长号规则
        assert_eq!(find_sensitive("12345678901234").len(), 1);
    }

    #[test]
    fn merges_overlapping_ranges() {
        // 17 位数字 + X：身份证与长号两个规则命中后应合并为一个区间
        let hits = find_sensitive("11010119900307451X");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0], (0, 18));
    }

    #[test]
    fn privacy_rects_union_words() {
        // 手机号被 OCR 拆成两个词，应合并为一个打码块
        let lines = vec![vec![
            word("电话", 0),
            word("138", 40),
            word("12345678", 80),
            word("完", 200),
        ]];
        let rects = privacy_rects(&lines);
        assert_eq!(rects.len(), 1);
        let r = rects[0];
        assert_eq!(r.x, 40);
        assert_eq!(r.x as u32 + r.w, 160);
    }

    #[test]
    fn clean_text_no_hits() {
        assert!(find_sensitive("普通的一行文字 hello world 2026").is_empty());
        assert!(privacy_rects(&[vec![word("你好世界", 0)]]).is_empty());
    }

    #[test]
    fn lines_join_with_space_and_newline() {
        let lines = vec![
            vec![word("hello", 0), word("world", 60)],
            vec![word("第二行", 0)],
        ];
        assert_eq!(lines_to_text(&lines), "hello world\n第二行");
        assert_eq!(lines_to_text(&[]), "");
    }

    #[test]
    fn cjk_words_join_without_spaces() {
        // 引擎把汉字逐个返回：不论词框间距（标点的墨迹很窄、间距会很大）都不补空格
        let line = vec![
            word("这", 0),
            word("是", 10),
            word("，", 20),
            word("中", 40),
            word("文", 50),
        ];
        assert_eq!(lines_to_text(&[line]), "这是，中文");
    }

    #[test]
    fn cjk_latin_boundary_uses_gap() {
        // 字高 20：间距 ≥ 4px 视为原文有空格
        let spaced = vec![word("使用", 0), word("Tauri", 26), word("构建", 82)];
        assert_eq!(lines_to_text(&[spaced]), "使用 Tauri 构建");
        let tight = vec![word("共", 0), word("3", 11), word("个", 22)];
        assert_eq!(lines_to_text(&[tight]), "共3个");
        // 中文标点紧邻英文：不补空格
        let punct = vec![word("你好", 0), word("，", 20), word("world", 40)];
        assert_eq!(lines_to_text(&[punct]), "你好，world");
    }

    #[test]
    fn latin_words_always_spaced() {
        // 英文词间距再小也补空格（引擎本身按空格分词）
        let line = vec![word("hello", 0), word("world", 50)];
        assert_eq!(lines_to_text(&[line]), "hello world");
        // 英文引号等共用符号不视为中文标点
        let quoted = vec![word("said", 0), word("“hi”", 50)];
        assert_eq!(lines_to_text(&[quoted]), "said “hi”");
    }
}
