//! 连通域分析模块。
//!
//! 采用 8 邻域广度优先搜索（BFS）标记连通域：一次遍历即可同时得到
//! 区域面积、包围盒和完整的像素列表，便于后续计算空间距离并精确清理。
//!
//! 约定：像素索引 = y * width + x，RGBA 数据按行连续存放。

use std::collections::VecDeque;

/// 8 邻域偏移量
const NEIGHBORS8: [(i32, i32); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// 像素包围盒（闭区间）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bbox {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Bbox {
    pub fn width(&self) -> u32 {
        self.x1 - self.x0 + 1
    }

    pub fn height(&self) -> u32 {
        self.y1 - self.y0 + 1
    }
}

/// 一个 8 邻域连通区域
#[derive(Debug)]
pub struct Component {
    /// 像素数量
    pub area: usize,
    /// 包围盒
    pub bbox: Bbox,
    /// 区域内最大 Alpha
    pub alpha_max: u8,
    /// 区域包含的全部像素索引
    pub pixels: Vec<u32>,
}

/// 一簇任意像素集合（用于统计被清除的杂色像素，不要求原图中连通）。
#[derive(Debug)]
pub struct PixelCluster {
    pub bbox: Bbox,
    pub pixels: Vec<u32>,
}

/// 把给定的像素索引按 8 邻域聚成簇，用于报告可追踪的清除区域。
pub fn group_pixels(pixels: &[u32], width: u32, height: u32) -> Vec<PixelCluster> {
    let w = width as usize;
    let n = w * height as usize;
    let mut mask = vec![false; n];
    for &p in pixels {
        mask[p as usize] = true;
    }

    let mut visited = vec![false; n];
    let mut clusters = Vec::new();
    let mut queue: VecDeque<u32> = VecDeque::new();

    for &start in pixels {
        let s = start as usize;
        if visited[s] {
            continue;
        }
        visited[s] = true;
        queue.clear();
        queue.push_back(start);

        let mut members = Vec::new();
        let (mut x0, mut y0) = (width, height);
        let (mut x1, mut y1) = (0u32, 0u32);

        while let Some(idx) = queue.pop_front() {
            let p = idx as usize;
            let x = (p % w) as u32;
            let y = (p / w) as u32;
            members.push(idx);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);

            for (dx, dy) in NEIGHBORS8 {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                    continue;
                }
                let np = ny as usize * w + nx as usize;
                if mask[np] && !visited[np] {
                    visited[np] = true;
                    queue.push_back(np as u32);
                }
            }
        }

        clusters.push(PixelCluster {
            bbox: Bbox { x0, y0, x1, y1 },
            pixels: members,
        });
    }

    clusters
}

/// 对 alpha >= threshold 的像素做 8 邻域连通域标记。
///
/// 完全透明的像素（alpha < threshold）不参与标记，也不会被计入任何区域。
pub fn label_components(
    rgba: &[u8],
    width: u32,
    height: u32,
    alpha_threshold: u8,
) -> Vec<Component> {
    let w = width as usize;
    let h = height as usize;
    let n = w * h;
    debug_assert_eq!(rgba.len(), n * 4, "RGBA 数据长度与图片尺寸不匹配");

    let mut visited = vec![false; n];
    let mut components = Vec::new();
    let mut queue: VecDeque<u32> = VecDeque::new();

    for start in 0..n {
        if visited[start] || rgba[start * 4 + 3] < alpha_threshold {
            continue;
        }

        visited[start] = true;
        queue.clear();
        queue.push_back(start as u32);

        let mut pixels: Vec<u32> = Vec::new();
        let mut alpha_max = 0u8;
        let mut min_x = width;
        let mut min_y = height;
        let mut max_x = 0u32;
        let mut max_y = 0u32;

        while let Some(idx) = queue.pop_front() {
            let p = idx as usize;
            let x = (p % w) as u32;
            let y = (p / w) as u32;

            pixels.push(idx);
            let a = rgba[p * 4 + 3];
            alpha_max = alpha_max.max(a);
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);

            for (dx, dy) in NEIGHBORS8 {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                    continue;
                }
                let np = ny as usize * w + nx as usize;
                if !visited[np] && rgba[np * 4 + 3] >= alpha_threshold {
                    visited[np] = true;
                    queue.push_back(np as u32);
                }
            }
        }

        components.push(Component {
            area: pixels.len(),
            bbox: Bbox {
                x0: min_x,
                y0: min_y,
                x1: max_x,
                y1: max_y,
            },
            alpha_max,
            pixels,
        });
    }

    components
}

/// 多源 BFS 距离变换：计算每个像素到最近源像素的切比雪夫距离（8 邻域步数）。
///
/// 源像素距离为 0。sources 为空的布尔掩码表示没有源，此时所有像素距离为 u32::MAX。
pub fn distance_field(width: u32, height: u32, sources: &[bool]) -> Vec<u32> {
    let w = width as usize;
    let h = height as usize;
    let n = w * h;
    let mut dist = vec![u32::MAX; n];
    let mut queue: VecDeque<u32> = VecDeque::new();

    for (i, &is_source) in sources.iter().enumerate().take(n) {
        if is_source {
            dist[i] = 0;
            queue.push_back(i as u32);
        }
    }

    while let Some(idx) = queue.pop_front() {
        let p = idx as usize;
        let next_dist = dist[p] + 1;
        let x = p % w;
        let y = p / w;

        for (dx, dy) in NEIGHBORS8 {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                continue;
            }
            let np = ny as usize * w + nx as usize;
            if dist[np] == u32::MAX {
                dist[np] = next_dist;
                queue.push_back(np as u32);
            }
        }
    }

    dist
}

fn flood_push(p: usize, rgba: &[u8], threshold: u8, exterior: &mut [bool], queue: &mut VecDeque<u32>) {
    if !exterior[p] && rgba[p * 4 + 3] < threshold {
        exterior[p] = true;
        queue.push_back(p as u32);
    }
}

/// 计算“外部背景”掩码：从图片四边出发、通过 4 邻域可达的透明像素。
///
/// 主体包围的封闭空洞不会被标记，从而可用于判断某个噪点是否位于主体内部。
pub fn exterior_background(
    rgba: &[u8],
    width: u32,
    height: u32,
    alpha_threshold: u8,
) -> Vec<bool> {
    let w = width as usize;
    let h = height as usize;
    let mut exterior = vec![false; w * h];
    if w == 0 || h == 0 {
        return exterior;
    }
    let mut queue: VecDeque<u32> = VecDeque::new();

    for x in 0..w {
        flood_push(x, rgba, alpha_threshold, &mut exterior, &mut queue);
        flood_push((h - 1) * w + x, rgba, alpha_threshold, &mut exterior, &mut queue);
    }
    for y in 0..h {
        flood_push(y * w, rgba, alpha_threshold, &mut exterior, &mut queue);
        flood_push(y * w + w - 1, rgba, alpha_threshold, &mut exterior, &mut queue);
    }

    while let Some(idx) = queue.pop_front() {
        let p = idx as usize;
        let x = p % w;
        let y = p / w;

        if x > 0 {
            flood_push(p - 1, rgba, alpha_threshold, &mut exterior, &mut queue);
        }
        if x + 1 < w {
            flood_push(p + 1, rgba, alpha_threshold, &mut exterior, &mut queue);
        }
        if y > 0 {
            flood_push(p - w, rgba, alpha_threshold, &mut exterior, &mut queue);
        }
        if y + 1 < h {
            flood_push(p + w, rgba, alpha_threshold, &mut exterior, &mut queue);
        }
    }

    exterior
}

/// 判断区域是否与外部背景接触（8 邻域）。
///
/// 若完全不接触，说明该区域被不透明主体包围，位于主体内部的封闭空洞中。
pub fn touches_exterior(
    component: &Component,
    exterior: &[bool],
    width: u32,
    height: u32,
) -> bool {
    let w = width as usize;

    for &idx in &component.pixels {
        let p = idx as usize;
        let x = p % w;
        let y = p / w;

        for (dx, dy) in NEIGHBORS8 {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                continue;
            }
            if exterior[ny as usize * w + nx as usize] {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造指定尺寸的 RGBA 数据，pixels 中的像素设为指定 alpha，RGB 固定为白色。
    fn rgba_with(w: u32, h: u32, pixels: &[(u32, u32, u8)]) -> Vec<u8> {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for &(x, y, a) in pixels {
            let p = ((y * w + x) * 4) as usize;
            data[p] = 255;
            data[p + 1] = 255;
            data[p + 2] = 255;
            data[p + 3] = a;
        }
        data
    }

    #[test]
    fn diagonal_pixels_belong_to_one_component() {
        let data = rgba_with(3, 3, &[(0, 0, 255), (1, 1, 255)]);
        let comps = label_components(&data, 3, 3, 1);
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].area, 2);
        assert_eq!(
            comps[0].bbox,
            Bbox { x0: 0, y0: 0, x1: 1, y1: 1 }
        );
    }

    #[test]
    fn distant_pixels_are_separate_components() {
        let data = rgba_with(3, 3, &[(0, 0, 255), (2, 2, 255)]);
        let comps = label_components(&data, 3, 3, 1);
        assert_eq!(comps.len(), 2);
    }

    #[test]
    fn alpha_threshold_filters_pixels() {
        let data = rgba_with(2, 2, &[(0, 0, 5), (1, 1, 255)]);
        let comps = label_components(&data, 2, 2, 10);
        assert_eq!(comps.len(), 1);
        assert_eq!(comps[0].area, 1);
    }

    #[test]
    fn distance_field_uses_chebyshev_metric() {
        let mut sources = vec![false; 25];
        sources[0] = true;
        let dist = distance_field(5, 5, &sources);
        assert_eq!(dist[0], 0);
        assert_eq!(dist[6], 1);
        assert_eq!(dist[24], 4);
    }

    #[test]
    fn enclosed_hole_is_not_exterior() {
        // 7x7：2..=4 的 3x3 不透明块，中心留一个透明洞
        let mut data = vec![0u8; 7 * 7 * 4];
        for y in 2..=4 {
            for x in 2..=4 {
                let p = (y * 7 + x) * 4;
                data[p] = 255;
                data[p + 3] = 255;
            }
        }
        let hole = (3 * 7 + 3) * 4;
        data[hole + 3] = 0;

        let exterior = exterior_background(&data, 7, 7, 1);
        assert!(exterior[0], "图片边缘的透明像素应属于外部背景");
        assert!(!exterior[(3 * 7 + 3) as usize], "被主体包围的透明洞不属于外部背景");
    }
}
