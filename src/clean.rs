//! 脏像素判定与清理：保守 / 激进两套策略。
//!
//! 核心原则：宁可漏掉无法确定的小噪点，也不误删游戏角色主体细节。
//! 因此候选区域必须同时满足以下条件才允许删除：
//!   1. 面积不超过 --max-area；
//!   2. 不与主体连通（连通域分析天然保证）；
//!   3. 与主体像素的距离不小于 --min-distance；
//!   4. 位于外部背景附近，而不是主体包围的封闭空洞内；
//!   5. 若设置了 --min-alpha，区域最大 Alpha 不超过该值。
//!
//! 清理动作只把目标像素的 Alpha 置 0，RGB 数据原样保留。

use crate::components::{
    Bbox, Component, distance_field, exterior_background, group_pixels, label_components,
    touches_exterior,
};
use image::RgbaImage;
use serde::Serialize;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// 保守清理：只处理明显孤立的小面积噪点
    Conservative,
    /// 激进清理：允许更大的孤立区域，但仍保护主体与主体内部
    Aggressive,
}

impl Mode {
    /// 各模式的默认最大清理面积
    pub fn default_max_area(self) -> u32 {
        match self {
            Mode::Conservative => 8,
            Mode::Aggressive => 64,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Conservative => "保守",
            Mode::Aggressive => "激进",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CleanOptions {
    pub mode: Mode,
    /// 最大可清理连通区域面积
    pub max_area: u32,
    /// Alpha 检测阈值：alpha >= 该值才视为有效像素
    pub alpha_threshold: u8,
    /// 低 Alpha 噪点过滤：仅清理最大 Alpha <= 该值的区域；0 表示不限制
    pub min_alpha: u8,
    /// 候选区域到主体像素的最小切比雪夫距离
    pub min_distance: u32,
    /// 附着低 Alpha 杂色清理上限：Alpha <= 该值且远离实心主体的像素会被清除；0 表示关闭
    pub fringe_alpha: u8,
    /// 低 Alpha 杂色到实心主体（Alpha >= 128）的最小切比雪夫距离
    pub fringe_distance: u32,
    /// 窄颈分离后，删除面积小于该值的附着小凸起；0 表示关闭
    pub prune_attached_area: u32,
}

/// “实心主体”的 Alpha 标准：达到该值的像素视为可见主体，作为距离参照。
/// 低 Alpha 杂色只有在远离实心主体一定距离时才清除，避免破坏正常抗锯齿边缘。
pub const CORE_ALPHA: u8 = 128;

/// 低 Alpha 残留区域的最大面积：整段都未达到可见不透明度的小区域视为脏像素。
/// 设置上限是为了保护“大面积但整体很淡”的合法光晕/阴影。
const FRINGE_MAX_DETACHED_AREA: usize = 1024;

/// 区域被保留的原因
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreserveReason {
    /// 面积超过 --max-area，属于主体或较大的独立元素
    TooLarge,
    /// 激进模式下没有明显主体时，用作参照的最大区域
    Anchor,
    /// 图片中没有足够大的主体，保守模式放弃清理
    NoAnchor,
    /// 区域最大 Alpha 超过 --min-alpha 限制
    AlphaTooHigh,
    /// 位于主体包围的封闭空洞内（主体内部）
    Enclosed,
    /// 与主体距离小于 --min-distance
    TooClose,
}

#[derive(Debug, Clone)]
pub struct RemovedRegion {
    pub bbox: Bbox,
    pub area: usize,
    pub alpha_max: u8,
    pub distance: u32,
}

#[derive(Debug, Clone)]
pub struct PreservedRegion {
    pub bbox: Bbox,
    pub area: usize,
    pub alpha_max: u8,
    pub distance: u32,
    pub reason: PreserveReason,
}

/// 通过形态学窄颈分离识别出的附着小凸起
#[derive(Debug, Clone)]
pub struct PruneRegion {
    pub bbox: Bbox,
    pub area: usize,
    pub alpha_max: u8,
}

/// 附着在主体上的低 Alpha 杂色带（按连通性聚合后的一段脏边）
#[derive(Debug, Clone)]
pub struct FringeRegion {
    pub bbox: Bbox,
    pub area: usize,
    pub alpha_max: u8,
    pub distance_min: u32,
    pub distance_max: u32,
}

#[derive(Debug)]
pub struct CleanResult {
    /// 清理后的图像（只改了目标像素的 Alpha）
    pub output: RgbaImage,
    pub components_total: usize,
    pub removed: Vec<RemovedRegion>,
    pub preserved: Vec<PreservedRegion>,
    /// 被清理的附着低 Alpha 杂色区域
    pub fringe: Vec<FringeRegion>,
    /// 被窄颈分离删除的附着小凸起
    pub pruned: Vec<PruneRegion>,
    /// 连通域清理掉的像素数
    pub component_pixels_removed: usize,
    /// 低 Alpha 杂色清理掉的像素数
    pub fringe_pixels_removed: usize,
    /// 窄颈分离清理掉的像素数
    pub prune_pixels_removed: usize,
    /// 合计清理像素数
    pub pixels_removed: usize,
}

/// 分析并清理图像。
pub fn clean(img: &RgbaImage, opts: &CleanOptions) -> CleanResult {
    let (width, height) = img.dimensions();
    let n = width as usize * height as usize;
    let components = label_components(img.as_raw(), width, height, opts.alpha_threshold);
    let max_area = opts.max_area as usize;

    // 1. 确定“主体参照区域”：面积大于阈值的连通域，任何情况下都不删除。
    let mut anchors: Vec<usize> = components
        .iter()
        .enumerate()
        .filter(|(_, c)| c.area > max_area)
        .map(|(i, _)| i)
        .collect();

    if anchors.is_empty() && opts.mode == Mode::Aggressive {
        // 激进模式下整张图都没有明显主体时，用最大区域作为参照，
        // 避免把画面里所有小元素都当成噪点清掉。
        if let Some((i, _)) = components.iter().enumerate().max_by_key(|(_, c)| c.area) {
            anchors.push(i);
        }
    }

    // 2. 一次多源 BFS 距离变换，得到每个像素到主体像素的切比雪夫距离。
    let has_anchor = !anchors.is_empty();
    let mut anchor_mask = vec![false; n];
    for &i in &anchors {
        for &p in &components[i].pixels {
            anchor_mask[p as usize] = true;
        }
    }
    let distances = if has_anchor {
        distance_field(width, height, &anchor_mask)
    } else {
        Vec::new()
    };

    // 3. 外部背景掩码，用于排除被主体包围的内部空洞。
    let exterior = exterior_background(img.as_raw(), width, height, opts.alpha_threshold);

    let mut output = img.clone();
    let data = output.as_mut();
    let mut removed = Vec::new();
    let mut preserved = Vec::new();
    let mut component_pixels_removed = 0usize;

    for (index, comp) in components.iter().enumerate() {
        let is_anchor = anchors.contains(&index);
        let distance = if has_anchor {
            min_distance_of(comp, &distances)
        } else {
            u32::MAX
        };

        // 保留原因按优先级判断；没有任何原因时才允许删除。
        let reason = if is_anchor {
            Some(if comp.area > max_area {
                PreserveReason::TooLarge
            } else {
                PreserveReason::Anchor
            })
        } else if !has_anchor {
            Some(PreserveReason::NoAnchor)
        } else if opts.min_alpha > 0 && comp.alpha_max > opts.min_alpha {
            Some(PreserveReason::AlphaTooHigh)
        } else if !touches_exterior(comp, &exterior, width, height) {
            Some(PreserveReason::Enclosed)
        } else if distance < opts.min_distance {
            Some(PreserveReason::TooClose)
        } else {
            None
        };

        if let Some(reason) = reason {
            preserved.push(PreservedRegion {
                bbox: comp.bbox,
                area: comp.area,
                alpha_max: comp.alpha_max,
                distance,
                reason,
            });
            continue;
        }

        // 仅将 Alpha 清零，RGB 原值保留；不触碰任何未命中的像素。
        for &p in &comp.pixels {
            data[p as usize * 4 + 3] = 0;
        }
        component_pixels_removed += comp.area;
        removed.push(RemovedRegion {
            bbox: comp.bbox,
            area: comp.area,
            alpha_max: comp.alpha_max,
            distance,
        });
    }

    // 4. 窄颈分离：删除通过细小颈部连在主体上的小凸起。
    //    做法是先用 3x3 腐蚀断开 1-2 像素宽的连接，再从剩余“核心”做测地重建，
    //    把每个像素归到最近的核心；重建面积小于阈值的核心区域视为附着噪点。
    let mut pruned = Vec::new();
    let mut prune_pixels_removed = 0usize;
    if opts.prune_attached_area > 0 {
        pruned = prune_attached(
            data,
            width,
            height,
            opts.alpha_threshold,
            opts.prune_attached_area as usize,
        );
        prune_pixels_removed = pruned.iter().map(|r| r.area).sum();
    }

    // 5. 清除附着在主体上的低 Alpha 杂色。
    //    这类像素与主体 8 邻域连通，无法通过连通域面积过滤识别；
    //    只能依据“Alpha 很低 + 离实心主体较远”来判断，正常 1-2 像素抗锯齿边缘会保留。
    let mut fringe = Vec::new();
    let mut fringe_pixels_removed = 0usize;
    if opts.fringe_alpha > 0 {
        // 用清理后的图像构建实心主体掩码，避免已被删除的小区域干扰距离计算。
        let mut core_mask = vec![false; n];
        let mut has_core = false;
        for i in 0..n {
            if data[i * 4 + 3] >= CORE_ALPHA {
                core_mask[i] = true;
                has_core = true;
            }
        }

        if has_core {
            let core_dist = distance_field(width, height, &core_mask);
            let mut fringe_pixels: Vec<u32> = Vec::new();
            for i in 0..n {
                let a = data[i * 4 + 3];
                // 杂色判定与连通域检测阈值无关：只要本身 Alpha 很低且远离实心主体就清除。
                if a > 0 && a <= opts.fringe_alpha && core_dist[i] >= opts.fringe_distance {
                    fringe_pixels.push(i as u32);
                }
            }

            for cluster in group_pixels(&fringe_pixels, width, height) {
                let mut alpha_max = 0u8;
                let mut distance_min = u32::MAX;
                let mut distance_max = 0u32;
                for &p in &cluster.pixels {
                    let p = p as usize;
                    alpha_max = alpha_max.max(data[p * 4 + 3]);
                    let d = core_dist[p];
                    distance_min = distance_min.min(d);
                    distance_max = distance_max.max(d);
                }
                fringe.push(FringeRegion {
                    bbox: cluster.bbox,
                    area: cluster.pixels.len(),
                    alpha_max,
                    distance_min,
                    distance_max,
                });
            }

            for &p in &fringe_pixels {
                data[p as usize * 4 + 3] = 0;
            }

            // 去除桥接像素后，可能留下“整段 Alpha 都很低”的分离细线/残点
            // （例如 1x11、Alpha 2-3 的彩色线）。它们离实心主体很近，单靠距离规则不会删除，
            // 但整段从未达到可见不透明度，属于脏像素，统一清除。
            let mut residual_pixels = 0usize;
            for comp in label_components(data, width, height, 1) {
                if comp.area <= FRINGE_MAX_DETACHED_AREA && comp.alpha_max <= opts.fringe_alpha {
                    let mut distance_min = u32::MAX;
                    let mut distance_max = 0u32;
                    for &p in &comp.pixels {
                        let d = core_dist[p as usize];
                        distance_min = distance_min.min(d);
                        distance_max = distance_max.max(d);
                    }
                    fringe.push(FringeRegion {
                        bbox: comp.bbox,
                        area: comp.area,
                        alpha_max: comp.alpha_max,
                        distance_min,
                        distance_max,
                    });
                    for &p in &comp.pixels {
                        data[p as usize * 4 + 3] = 0;
                    }
                    residual_pixels += comp.area;
                }
            }

            fringe_pixels_removed = fringe_pixels.len() + residual_pixels;
        }
    }

    removed.sort_by_key(|r| (r.bbox.y0, r.bbox.x0));
    preserved.sort_by_key(|r| std::cmp::Reverse(r.area));
    fringe.sort_by_key(|r| (r.bbox.y0, r.bbox.x0));

    CleanResult {
        output,
        components_total: components.len(),
        removed,
        preserved,
        fringe,
        pruned,
        component_pixels_removed,
        fringe_pixels_removed,
        prune_pixels_removed,
        pixels_removed: component_pixels_removed + fringe_pixels_removed + prune_pixels_removed,
    }
}

/// 窄颈分离并删除小凸起。
///
/// 只处理与大型区域通过 1-2 像素宽颈部相连的凸起；宽基底融合的色块在腐蚀后
/// 仍与主体核心连在一起，不会被误删（也无法在无形状语义的情况下分割）。
fn prune_attached(
    data: &mut [u8],
    width: u32,
    height: u32,
    alpha_threshold: u8,
    min_area: usize,
) -> Vec<PruneRegion> {
    let w = width as usize;
    let h = height as usize;
    let n = w * h;

    let mut foreground = vec![false; n];
    for i in 0..n {
        foreground[i] = data[i * 4 + 3] >= alpha_threshold;
    }

    // 3x3 腐蚀：8 邻域全部为前景才保留。图像边界像素一律腐蚀掉。
    let mut eroded = vec![false; n];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let p = y * w + x;
            if !foreground[p] {
                continue;
            }
            let mut keep = true;
            for (dx, dy) in [(-1i32, -1i32), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let np = (y as i32 + dy) as usize * w + (x as i32 + dx) as usize;
                if !foreground[np] {
                    keep = false;
                    break;
                }
            }
            eroded[p] = keep;
        }
    }

    let seeds: Vec<u32> = (0..n).filter(|&i| eroded[i]).map(|i| i as u32).collect();
    if seeds.is_empty() {
        return Vec::new();
    }

    let mut cores = group_pixels(&seeds, width, height);
    // 只有一个核心时说明整张图是一个整体，没有可分离的凸起。
    if cores.len() <= 1 {
        return Vec::new();
    }
    // 小核心优先扩展：让细小凸起认领连接它的窄颈，避免删掉凸起后留下 1 像素残桩。
    cores.sort_by_key(|core| core.pixels.len());

    // 从所有核心做多源 BFS，把每个前景像素归到最近的核心。
    let mut labels = vec![u32::MAX; n];
    let mut queue: VecDeque<u32> = VecDeque::new();
    for (index, core) in cores.iter().enumerate() {
        let label = index as u32;
        for &p in &core.pixels {
            labels[p as usize] = label;
            queue.push_back(p);
        }
    }
    while let Some(idx) = queue.pop_front() {
        let p = idx as usize;
        let label = labels[p];
        let x = p % w;
        let y = p / w;
        for (dx, dy) in [(-1i32, -1i32), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                continue;
            }
            let np = ny as usize * w + nx as usize;
            if foreground[np] && labels[np] == u32::MAX {
                labels[np] = label;
                queue.push_back(np as u32);
            }
        }
    }

    let k = cores.len();
    let mut areas = vec![0usize; k];
    let mut bboxes = vec![(u32::MAX, u32::MAX, 0u32, 0u32); k];
    let mut alpha_max = vec![0u8; k];
    let mut label_pixels: Vec<Vec<u32>> = vec![Vec::new(); k];

    for i in 0..n {
        let label = labels[i];
        if label == u32::MAX {
            continue;
        }
        let li = label as usize;
        let x = (i % w) as u32;
        let y = (i / w) as u32;
        areas[li] += 1;
        label_pixels[li].push(i as u32);
        alpha_max[li] = alpha_max[li].max(data[i * 4 + 3]);
        let bbox = &mut bboxes[li];
        bbox.0 = bbox.0.min(x);
        bbox.1 = bbox.1.min(y);
        bbox.2 = bbox.2.max(x);
        bbox.3 = bbox.3.max(y);
    }

    // 保护最大的重建区域，避免小图或整体很细的素材被全部删除。
    let largest = areas.iter().copied().max().unwrap_or(0);
    let mut regions = Vec::new();
    for li in 0..k {
        if areas[li] == 0 || areas[li] >= min_area || areas[li] == largest {
            continue;
        }
        for &p in &label_pixels[li] {
            data[p as usize * 4 + 3] = 0;
        }
        let (x0, y0, x1, y1) = bboxes[li];
        regions.push(PruneRegion {
            bbox: Bbox { x0, y0, x1, y1 },
            area: areas[li],
            alpha_max: alpha_max[li],
        });
    }
    regions.sort_by_key(|r| (r.bbox.y0, r.bbox.x0));
    regions
}

fn min_distance_of(comp: &Component, distances: &[u32]) -> u32 {
    comp.pixels
        .iter()
        .map(|&p| distances[p as usize])
        .min()
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn blank(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([100, 150, 200, 0]))
    }

    fn set(img: &mut RgbaImage, x: u32, y: u32, rgba: [u8; 4]) {
        img.put_pixel(x, y, Rgba(rgba));
    }

    fn options(mode: Mode) -> CleanOptions {
        CleanOptions {
            mode,
            max_area: mode.default_max_area(),
            alpha_threshold: 1,
            min_alpha: 0,
            min_distance: 2,
            fringe_alpha: 16,
            fringe_distance: 3,
            prune_attached_area: 0,
        }
    }

    fn solid_body(img: &mut RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                set(img, x, y, [10, 20, 30, 255]);
            }
        }
    }

    #[test]
    fn removes_isolated_speck_and_keeps_body_untouched() {
        let mut img = blank(40, 40);
        solid_body(&mut img, 10, 10, 29, 29);
        set(&mut img, 2, 2, [200, 0, 0, 1]);

        let before = img.clone();
        let result = clean(&img, &options(Mode::Conservative));

        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.pixels_removed, 1);
        assert_eq!(result.removed[0].distance, 8);

        // 噪点 Alpha 清零，RGB 保留
        let after = result.output.get_pixel(2, 2);
        assert_eq!(after.0[3], 0);
        assert_eq!(&after.0[..3], &before.get_pixel(2, 2).0[..3]);

        // 主体像素完全不变
        for y in 10..=29 {
            for x in 10..=29 {
                assert_eq!(result.output.get_pixel(x, y), before.get_pixel(x, y));
            }
        }
    }

    #[test]
    fn only_target_alpha_is_cleared() {
        let mut img = blank(30, 30);
        solid_body(&mut img, 5, 5, 24, 24);
        set(&mut img, 1, 1, [1, 2, 3, 200]);
        set(&mut img, 27, 27, [4, 5, 6, 128]);

        let before = img.clone();
        let result = clean(&img, &options(Mode::Conservative));
        assert_eq!(result.removed.len(), 2);

        for (bp, ap) in before.pixels().zip(result.output.pixels()) {
            if bp != ap {
                assert!(bp.0[3] > 0 && ap.0[3] == 0, "只允许把 Alpha 清零");
                assert_eq!(&bp.0[..3], &ap.0[..3], "RGB 不允许改变");
            }
        }
    }

    #[test]
    fn speck_inside_enclosed_hole_is_preserved() {
        let mut img = blank(40, 40);
        // 只画 10..=29 的边框，内部形成封闭空洞
        for y in 10..=29 {
            for x in 10..=29 {
                if x == 10 || x == 29 || y == 10 || y == 29 {
                    set(&mut img, x, y, [10, 20, 30, 255]);
                }
            }
        }
        set(&mut img, 20, 20, [255, 255, 255, 200]);

        let result = clean(&img, &options(Mode::Conservative));
        assert_eq!(result.removed.len(), 0);
        let kept = result
            .preserved
            .iter()
            .find(|p| p.bbox.x0 == 20 && p.bbox.y0 == 20)
            .expect("内部噪点应保留在报告中");
        assert_eq!(kept.reason, PreserveReason::Enclosed);
    }

    #[test]
    fn speck_too_close_to_body_is_preserved() {
        let mut img = blank(40, 40);
        solid_body(&mut img, 10, 10, 29, 29);
        set(&mut img, 11, 8, [255, 255, 255, 1]);

        let mut opts = options(Mode::Conservative);
        opts.min_distance = 3;
        let result = clean(&img, &opts);

        assert_eq!(result.removed.len(), 0);
        let kept = result
            .preserved
            .iter()
            .find(|p| p.bbox.x0 == 11 && p.bbox.y0 == 8)
            .expect("近距噪点应保留");
        assert_eq!(kept.reason, PreserveReason::TooClose);
    }

    #[test]
    fn min_alpha_filter_protects_high_alpha_specks() {
        let mut img = blank(40, 40);
        solid_body(&mut img, 10, 10, 29, 29);
        set(&mut img, 2, 2, [255, 0, 0, 200]);

        let mut opts = options(Mode::Conservative);
        opts.min_alpha = 16;
        let result = clean(&img, &opts);

        assert_eq!(result.removed.len(), 0);
        let kept = result
            .preserved
            .iter()
            .find(|p| p.bbox.x0 == 2 && p.bbox.y0 == 2)
            .expect("高 Alpha 区域应保留");
        assert_eq!(kept.reason, PreserveReason::AlphaTooHigh);
    }

    #[test]
    fn area_above_limit_is_preserved() {
        let mut img = blank(40, 40);
        solid_body(&mut img, 10, 10, 29, 29);
        set(&mut img, 2, 2, [255, 0, 0, 255]);
        set(&mut img, 3, 2, [255, 0, 0, 255]);

        let mut opts = options(Mode::Conservative);
        opts.max_area = 1;
        let result = clean(&img, &opts);

        assert_eq!(result.removed.len(), 0);
        let kept = result
            .preserved
            .iter()
            .find(|p| p.bbox.x0 == 2 && p.bbox.y0 == 2)
            .expect("面积超限的区域应保留");
        assert_eq!(kept.reason, PreserveReason::TooLarge);
    }

    #[test]
    fn attached_low_alpha_fringe_is_removed() {
        let mut img = blank(40, 40);
        solid_body(&mut img, 10, 10, 29, 29);
        // 从主体左侧延伸出 4 个 Alpha=1 的像素，模拟与主体连通的低 Alpha 脏边
        set(&mut img, 9, 15, [255, 255, 255, 1]);
        set(&mut img, 8, 15, [255, 255, 255, 1]);
        set(&mut img, 7, 15, [255, 255, 255, 1]);
        set(&mut img, 6, 15, [255, 255, 255, 1]);

        let result = clean(&img, &options(Mode::Conservative));

        // 连通域阶段不会删除任何东西（它们属于主体连通域）
        assert_eq!(result.component_pixels_removed, 0);
        // 距实心主体 1-2 像素的正常过渡保留，3-4 像素的低 Alpha 杂色清除
        assert_eq!(result.fringe_pixels_removed, 2);
        assert_eq!(result.output.get_pixel(7, 15).0[3], 0);
        assert_eq!(result.output.get_pixel(6, 15).0[3], 0);
        assert_eq!(result.output.get_pixel(8, 15).0[3], 1);
        assert_eq!(result.output.get_pixel(9, 15).0[3], 1);

        assert_eq!(result.fringe.len(), 1);
        let region = &result.fringe[0];
        assert_eq!(region.area, 2);
        assert_eq!(region.alpha_max, 1);
        assert_eq!(region.distance_min, 3);
        assert_eq!(region.distance_max, 4);
    }

    #[test]
    fn fringe_cleanup_can_be_disabled() {
        let mut img = blank(40, 40);
        solid_body(&mut img, 10, 10, 29, 29);
        // 与主体连成一体，确保连通域阶段不会删除，只受 fringe 开关影响
        set(&mut img, 9, 15, [255, 255, 255, 1]);
        set(&mut img, 8, 15, [255, 255, 255, 1]);
        set(&mut img, 7, 15, [255, 255, 255, 1]);

        let mut opts = options(Mode::Conservative);
        opts.fringe_alpha = 0;
        let result = clean(&img, &opts);

        assert_eq!(result.fringe_pixels_removed, 0);
        assert_eq!(result.output.get_pixel(7, 15).0[3], 1);
    }

    #[test]
    fn detached_low_alpha_dust_is_removed_by_fringe_stage() {
        let mut img = blank(40, 40);
        solid_body(&mut img, 10, 10, 29, 29);
        // 与主体分离的 3x3 低 Alpha 尘点，面积 9 超过 max-area，但属于低 Alpha 杂色
        for y in 2..=4 {
            for x in 2..=4 {
                set(&mut img, x, y, [255, 255, 255, 1]);
            }
        }

        let result = clean(&img, &options(Mode::Conservative));
        assert_eq!(result.component_pixels_removed, 0);
        assert_eq!(result.fringe_pixels_removed, 9);
        for y in 2..=4 {
            for x in 2..=4 {
                assert_eq!(result.output.get_pixel(x, y).0[3], 0);
            }
        }
    }

    #[test]
    fn prune_attached_removes_solid_small_blob_with_narrow_neck() {
        let mut img = blank(60, 60);
        solid_body(&mut img, 10, 10, 29, 29);
        // 1 像素实心颈部 + 3x3 实心小凸起，Alpha 全为 255
        set(&mut img, 30, 20, [255, 0, 0, 255]);
        for y in 19..=21 {
            for x in 31..=33 {
                set(&mut img, x, y, [255, 0, 0, 255]);
            }
        }

        // 关闭时不会被普通连通域规则删除
        let result = clean(&img, &options(Mode::Conservative));
        assert_eq!(result.output.get_pixel(32, 20).0[3], 255);
        assert_eq!(result.prune_pixels_removed, 0);

        // 开启窄颈分离后，3x3 凸起及颈部被删除，主体保持完整
        let mut opts = options(Mode::Conservative);
        opts.prune_attached_area = 16;
        let result = clean(&img, &opts);
        for y in 19..=21 {
            for x in 31..=33 {
                assert_eq!(result.output.get_pixel(x, y).0[3], 0, "小凸起应被删除");
            }
        }
        assert_eq!(result.output.get_pixel(30, 20).0[3], 0, "窄颈残桩也应被删除");
        assert_eq!(result.output.get_pixel(29, 20).0[3], 255, "主体不应被改动");
        assert_eq!(result.prune_pixels_removed, 10);
        assert_eq!(result.pruned.len(), 1);
    }

    #[test]
    fn prune_attached_keeps_largest_region() {
        let mut img = blank(20, 20);
        solid_body(&mut img, 5, 5, 9, 9);
        // 与主体分离的小块也参与重建，但最大区域必须保留
        set(&mut img, 15, 15, [255, 0, 0, 255]);

        let mut opts = options(Mode::Conservative);
        opts.prune_attached_area = 1000;
        let result = clean(&img, &opts);
        assert_eq!(result.output.get_pixel(7, 7).0[3], 255, "最大区域必须保留");
    }

    #[test]
    fn raising_alpha_threshold_separates_low_alpha_bridge() {
        let mut img = blank(50, 50);
        solid_body(&mut img, 10, 10, 29, 29);
        // 1 像素低 Alpha 桥连接一个 3x3 不透明小色块
        set(&mut img, 31, 20, [255, 0, 0, 10]);
        for y in 19..=21 {
            for x in 32..=34 {
                set(&mut img, x, y, [255, 0, 0, 255]);
            }
        }

        // 默认阈值 1：小色块通过低 Alpha 桥与主体连通，属于主体连通域，不会被面积规则删除
        let mut opts = options(Mode::Conservative);
        opts.max_area = 8;
        let result = clean(&img, &opts);
        assert_eq!(result.output.get_pixel(33, 20).0[3], 255);

        // 提高阈值到 32：低 Alpha 桥被视为背景，3x3 色块成为独立连通域，面积 9 <= 16 被删除
        opts.alpha_threshold = 32;
        opts.max_area = 16;
        let result = clean(&img, &opts);
        assert_eq!(result.output.get_pixel(33, 20).0[3], 0);
        assert!(result.removed.iter().any(|r| r.area == 9 && r.bbox.x0 == 32));
    }

    #[test]
    fn near_body_low_alpha_line_is_removed_as_residual() {
        let mut img = blank(40, 40);
        solid_body(&mut img, 10, 10, 29, 29);
        // 与主体相距 2 像素、面积 11 的细线：连通域阶段因面积超限保留，
        // 距离规则也不会删除，但它整段 Alpha 只有 2，应作为残留清除。
        for y in 15..=25 {
            set(&mut img, 8, y, [255, 255, 255, 2]);
        }

        let result = clean(&img, &options(Mode::Conservative));
        assert_eq!(result.component_pixels_removed, 0);
        assert_eq!(result.fringe_pixels_removed, 11);
        for y in 15..=25 {
            assert_eq!(result.output.get_pixel(8, y).0[3], 0);
        }
        assert!(result.fringe.iter().any(|f| f.area == 11 && f.alpha_max == 2));
    }

    #[test]
    fn conservative_without_anchor_removes_nothing() {
        let mut img = blank(20, 20);
        set(&mut img, 1, 1, [255, 255, 255, 255]);

        let result = clean(&img, &options(Mode::Conservative));
        assert_eq!(result.removed.len(), 0);
        assert_eq!(result.preserved.len(), 1);
        assert_eq!(result.preserved[0].reason, PreserveReason::NoAnchor);
    }
}
