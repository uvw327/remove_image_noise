//! JSON 检测报告的数据结构。
//!
//! 报告记录每个被清理区域的坐标、面积、最大 Alpha 和到主体的距离，
//! 保证所有删除操作都可以追踪复核。

use crate::clean::{CleanOptions, CleanResult, Mode, PreserveReason};
use crate::components::Bbox;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct RegionReport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub area: usize,
    pub alpha_max: u8,
    pub distance: u32,
}

#[derive(Debug, Serialize)]
pub struct PreservedReport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub area: usize,
    pub alpha_max: u8,
    pub distance: u32,
    pub reason: PreserveReason,
}

/// 被清除的附着低 Alpha 杂色带
#[derive(Debug, Serialize)]
pub struct FringeReport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub area: usize,
    pub alpha_max: u8,
    pub distance_min: u32,
    pub distance_max: u32,
}

/// 通过窄颈分离删除的附着小凸起
#[derive(Debug, Serialize)]
pub struct PruneReport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub area: usize,
    pub alpha_max: u8,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    pub dry_run: bool,
    pub width: u32,
    pub height: u32,
    pub mode: Mode,
    pub max_area: u32,
    pub alpha_threshold: u8,
    pub min_alpha: u8,
    pub min_distance: u32,
    pub fringe_alpha: u8,
    pub fringe_distance: u32,
    pub prune_attached_area: u32,
    pub components_total: usize,
    pub components_removed: usize,
    pub components_preserved: usize,
    pub pixels_removed: usize,
    pub pixels_removed_components: usize,
    pub pixels_removed_fringe: usize,
    pub pixels_removed_prune: usize,
    pub removed_regions: Vec<RegionReport>,
    pub preserved_regions: Vec<PreservedReport>,
    pub fringe_regions: Vec<FringeReport>,
    pub pruned_regions: Vec<PruneReport>,
}

impl Report {
    pub fn build(
        input: String,
        output: Option<String>,
        width: u32,
        height: u32,
        opts: &CleanOptions,
        result: &CleanResult,
        dry_run: bool,
    ) -> Self {
        let removed_regions = result
            .removed
            .iter()
            .map(|r| region(r.bbox, r.area, r.alpha_max, r.distance))
            .collect();

        let preserved_regions = result
            .preserved
            .iter()
            .map(|p| PreservedReport {
                x: p.bbox.x0,
                y: p.bbox.y0,
                width: p.bbox.width(),
                height: p.bbox.height(),
                area: p.area,
                alpha_max: p.alpha_max,
                distance: p.distance,
                reason: p.reason,
            })
            .collect();

        let pruned_regions = result
            .pruned
            .iter()
            .map(|p| PruneReport {
                x: p.bbox.x0,
                y: p.bbox.y0,
                width: p.bbox.width(),
                height: p.bbox.height(),
                area: p.area,
                alpha_max: p.alpha_max,
            })
            .collect();

        let fringe_regions = result
            .fringe
            .iter()
            .map(|f| FringeReport {
                x: f.bbox.x0,
                y: f.bbox.y0,
                width: f.bbox.width(),
                height: f.bbox.height(),
                area: f.area,
                alpha_max: f.alpha_max,
                distance_min: f.distance_min,
                distance_max: f.distance_max,
            })
            .collect();

        Self {
            input,
            output,
            dry_run,
            width,
            height,
            mode: opts.mode,
            max_area: opts.max_area,
            alpha_threshold: opts.alpha_threshold,
            min_alpha: opts.min_alpha,
            min_distance: opts.min_distance,
            fringe_alpha: opts.fringe_alpha,
            fringe_distance: opts.fringe_distance,
            prune_attached_area: opts.prune_attached_area,
            components_total: result.components_total,
            components_removed: result.removed.len(),
            components_preserved: result.components_total - result.removed.len(),
            pixels_removed: result.pixels_removed,
            pixels_removed_components: result.component_pixels_removed,
            pixels_removed_fringe: result.fringe_pixels_removed,
            pixels_removed_prune: result.prune_pixels_removed,
            removed_regions,
            preserved_regions,
            fringe_regions,
            pruned_regions,
        }
    }
}

fn region(bbox: Bbox, area: usize, alpha_max: u8, distance: u32) -> RegionReport {
    RegionReport {
        x: bbox.x0,
        y: bbox.y0,
        width: bbox.width(),
        height: bbox.height(),
        area,
        alpha_max,
        distance,
    }
}

/// 批量模式下单张图片的结果（成功记录报告，失败记录错误，不影响其他图片）
#[derive(Debug, Serialize)]
pub struct BatchItem {
    pub input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
}
