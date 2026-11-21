//! 诊断工具：分析“与主体连通”的低 Alpha 杂色/毛刺。
//!
//! 做法：以 alpha >= 128 的像素为“实心主体”，计算每个像素到实心主体的
//! 切比雪夫距离，然后按 Alpha 桶统计距离分布。正常抗锯齿像素应紧贴实心主体
//! （距离 1-2），若低 Alpha 像素延伸很远，则说明是附着在主体上的脏像素。
//!
//! 用法：cargo run --release --example diagnose_fringe -- output/a.png output/fringe.png

use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};
use std::collections::VecDeque;

const NEIGHBORS8: [(i32, i32); 8] = [
    (-1, -1), (0, -1), (1, -1),
    (-1, 0),           (1, 0),
    (-1, 1),  (0, 1),  (1, 1),
];

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| "output/a.png".into());
    let marked_path = args.next().unwrap_or_else(|| "output/fringe.png".into());
    let img = image::open(&input).with_context(|| format!("无法打开 {input}"))?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let (w, h) = (width as usize, height as usize);
    let data = rgba.as_raw();

    // 实心主体 = alpha >= 128
    let mut core = vec![false; w * h];
    for (i, px) in data.as_chunks::<4>().0.iter().enumerate() {
        core[i] = px[3] >= 128;
    }
    let core_count = core.iter().filter(|&&v| v).count();

    // 到实心主体的距离
    let mut dist = vec![u32::MAX; w * h];
    let mut queue: VecDeque<u32> = VecDeque::new();
    for (i, &is_core) in core.iter().enumerate() {
        if is_core {
            dist[i] = 0;
            queue.push_back(i as u32);
        }
    }
    while let Some(idx) = queue.pop_front() {
        let p = idx as usize;
        let nd = dist[p] + 1;
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
                dist[np] = nd;
                queue.push_back(np as u32);
            }
        }
    }

    println!("文件: {input}  {width}x{height}");
    println!("实心主体(alpha>=128): {core_count} 像素");
    println!("Alpha 桶 -> 到实心主体的距离分布（只统计非零）:");
    let buckets: [(u8, u8); 7] = [
        (1, 4),
        (5, 16),
        (17, 32),
        (33, 64),
        (65, 127),
        (128, 200),
        (201, 255),
    ];
    for (lo, hi) in buckets {
        let mut by_dist = std::collections::BTreeMap::<u32, usize>::new();
        let mut total = 0usize;
        for (i, px) in data.as_chunks::<4>().0.iter().enumerate() {
            let a = px[3];
            if a >= lo && a <= hi {
                total += 1;
                *by_dist.entry(dist[i]).or_insert(0) += 1;
            }
        }
        if total == 0 {
            continue;
        }
        let mut parts = Vec::new();
        for (d, c) in &by_dist {
            parts.push(format!("d{d}={c}"));
        }
        println!("  alpha {lo}-{hi}: 共 {total}  ->  {}", parts.join(", "));
    }

    // 可疑像素：低 Alpha 且离实心主体较远
    let is_suspicious = |i: usize| -> bool {
        let a = data[i * 4 + 3];
        ((1..=32).contains(&a) && dist[i] >= 3) || ((33..=127).contains(&a) && dist[i] >= 4)
    };
    let suspicious: Vec<usize> = (0..w * h).filter(|&i| is_suspicious(i)).collect();
    println!("可疑附着杂色像素: {} 个", suspicious.len());

    // 可疑像素的 Alpha / 距离 / RGB 分布
    let mut susp_alpha = std::collections::BTreeMap::<u8, usize>::new();
    let mut saturation_sum = 0u64;
    let mut alpha_sum = 0u64;
    for &i in &suspicious {
        let px = &data[i * 4..i * 4 + 4];
        *susp_alpha.entry(px[3]).or_insert(0) += 1;
        let max = px[0].max(px[1]).max(px[2]);
        let min = px[0].min(px[1]).min(px[2]);
        saturation_sum += (max - min) as u64;
        alpha_sum += px[3] as u64;
    }
    println!("可疑像素 Alpha 分布: {susp_alpha:?}");
    if !suspicious.is_empty() {
        println!(
            "可疑像素平均 Alpha: {:.2}，平均饱和度(max-min): {:.2}",
            alpha_sum as f64 / suspicious.len() as f64,
            saturation_sum as f64 / suspicious.len() as f64
        );
        for &i in suspicious.iter().take(10) {
            let px = &data[i * 4..i * 4 + 4];
            println!(
                "  样本 ({}, {}) rgba=({},{},{},{}) d={}",
                i % w, i / w, px[0], px[1], px[2], px[3], dist[i]
            );
        }
    }

    // 对可疑像素做 8 邻域聚类，看看它们是环形光晕还是散点
    let susp_mask: Vec<bool> = (0..w * h).map(is_suspicious).collect();
    let mut seen = vec![false; w * h];
    let mut clusters: Vec<(usize, u32, u32, u32, u32)> = Vec::new();
    for start in 0..w * h {
        if !susp_mask[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut q = VecDeque::new();
        q.push_back(start as u32);
        let (mut area, mut x0, mut y0, mut x1, mut y1) = (0usize, u32::MAX, u32::MAX, 0u32, 0u32);
        while let Some(idx) = q.pop_front() {
            let p = idx as usize;
            area += 1;
            let x = (p % w) as u32;
            let y = (p / w) as u32;
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
                if susp_mask[np] && !seen[np] {
                    seen[np] = true;
                    q.push_back(np as u32);
                }
            }
        }
        clusters.push((area, x0, y0, x1, y1));
    }
    clusters.sort_by_key(|c| std::cmp::Reverse(c.0));
    println!("可疑像素聚成 {} 个 8 邻域簇，最大 10 个:", clusters.len());
    for (area, x0, y0, x1, y1) in clusters.iter().take(10) {
        println!(
            "  area={area} bbox=({x0},{y0})-({x1},{y1}) {}x{}",
            x1 - x0 + 1,
            y1 - y0 + 1
        );
    }

    // 输出叠加图：棋盘格 + 原图 + 可疑像素标红
    let mut out = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let c = if (x / 8 + y / 8) % 2 == 0 { 225u8 } else { 195u8 };
            out.put_pixel(x, y, Rgba([c, c, c, 255]));
        }
    }
    for y in 0..height {
        for x in 0..width {
            let p = rgba.get_pixel(x, y).0;
            if p[3] == 0 {
                continue;
            }
            let a = p[3] as u32;
            let bg = out.get_pixel(x, y).0;
            let blend = |fg: u8, bg: u8| ((fg as u32 * a + bg as u32 * (255 - a)) / 255) as u8;
            out.put_pixel(x, y, Rgba([
                blend(p[0], bg[0]),
                blend(p[1], bg[1]),
                blend(p[2], bg[2]),
                255,
            ]));
        }
    }
    for &i in &suspicious {
        let x = (i % w) as u32;
        let y = (i / w) as u32;
        out.put_pixel(x, y, Rgba([255, 0, 0, 255]));
    }
    out.save_with_format(&marked_path, image::ImageFormat::Png)
        .with_context(|| format!("写入 {marked_path} 失败"))?;
    println!("诊断图: {marked_path}");

    Ok(())
}
