//! 噪点可视化：把被清理的像素标成红色，并绘制区域外框，便于人工复核误判。
//!
//! 背景使用棋盘格代替透明，避免小噪点在查看器中不可见。

use crate::clean::RemovedRegion;
use image::{Rgba, RgbaImage};

const CHECKER_LIGHT: [u8; 3] = [225, 225, 225];
const CHECKER_DARK: [u8; 3] = [195, 195, 195];
const TILE: u32 = 8;

pub fn render(before: &RgbaImage, after: &RgbaImage, removed: &[RemovedRegion]) -> RgbaImage {
    let (width, height) = before.dimensions();
    let mut out = RgbaImage::new(width, height);

    // 1. 棋盘格背景
    for y in 0..height {
        for x in 0..width {
            let color = if (x / TILE + y / TILE).is_multiple_of(2) {
                CHECKER_LIGHT
            } else {
                CHECKER_DARK
            };
            out.put_pixel(x, y, Rgba([color[0], color[1], color[2], 255]));
        }
    }

    // 2. 按 Alpha 把原图混合到棋盘格上
    for y in 0..height {
        for x in 0..width {
            let src = before.get_pixel(x, y).0;
            if src[3] == 0 {
                continue;
            }
            let alpha = src[3] as u32;
            let bg = out.get_pixel(x, y).0;
            let blend = |fg: u8, bg: u8| {
                ((fg as u32 * alpha + bg as u32 * (255 - alpha)) / 255) as u8
            };
            out.put_pixel(
                x,
                y,
                Rgba([
                    blend(src[0], bg[0]),
                    blend(src[1], bg[1]),
                    blend(src[2], bg[2]),
                    255,
                ]),
            );
        }
    }

    // 3. 被清理的像素标为纯红
    for y in 0..height {
        for x in 0..width {
            if before.get_pixel(x, y).0[3] > 0 && after.get_pixel(x, y).0[3] == 0 {
                out.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
    }

    // 4. 区域外框向外扩 2 像素画红框，让单像素噪点也醒目
    for region in removed {
        let x0 = region.bbox.x0.saturating_sub(2);
        let y0 = region.bbox.y0.saturating_sub(2);
        let x1 = (region.bbox.x1 + 2).min(width.saturating_sub(1));
        let y1 = (region.bbox.y1 + 2).min(height.saturating_sub(1));

        // 只在背景上画框，避免红框覆盖主体像素造成误判。
        let mut paint = |x: u32, y: u32| {
            if after.get_pixel(x, y).0[3] == 0 {
                out.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        };
        for x in x0..=x1 {
            paint(x, y0);
            paint(x, y1);
        }
        for y in y0..=y1 {
            paint(x0, y);
            paint(x1, y);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Bbox;

    #[test]
    fn render_keeps_size_and_marks_pixels_red() {
        let mut before = RgbaImage::new(10, 10);
        before.put_pixel(3, 4, Rgba([10, 20, 30, 5]));
        let mut after = before.clone();
        after.put_pixel(3, 4, Rgba([10, 20, 30, 0]));

        let removed = vec![RemovedRegion {
            bbox: Bbox { x0: 3, y0: 4, x1: 3, y1: 4 },
            area: 1,
            alpha_max: 5,
            distance: 9,
        }];

        let marked = render(&before, &after, &removed);
        assert_eq!(marked.dimensions(), (10, 10));
        assert_eq!(marked.get_pixel(3, 4), &Rgba([255, 0, 0, 255]));
        assert_eq!(marked.get_pixel(0, 0).0[3], 255, "背景应为不透明棋盘格");
    }
}
