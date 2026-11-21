//! 临时图像分析工具：用于在设计算法前实际检查 temp/a.png 的像素与连通域分布。
//! 用法：cargo run --example inspect -- temp/a.png

#![allow(clippy::type_complexity)]

use anyhow::{Context, Result};
use image::GenericImageView;
use std::collections::VecDeque;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "temp/a.png".into());
    let img = image::open(&path).with_context(|| format!("无法打开图片: {path}"))?;
    let (w, h) = img.dimensions();
    println!("文件: {path}");
    println!("尺寸: {w} x {h}");
    println!("原始色彩类型: {:?}", img.color());
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let data = rgba.as_raw();

    // --- Alpha 分布 ---
    let mut alpha_hist = [0u64; 256];
    let mut opaque = 0u64;
    let mut non_transparent = 0u64; // alpha >= 1
    for px in data.as_chunks::<4>().0 {
        let a = px[3];
        alpha_hist[a as usize] += 1;
        if a == 255 {
            opaque += 1;
        }
        if a >= 1 {
            non_transparent += 1;
        }
    }
    println!("总像素: {}", w as u64 * h as u64);
    println!("alpha==0: {}", alpha_hist[0]);
    println!("alpha==255: {opaque}");
    println!("alpha>=1 (非透明): {non_transparent}");
    println!("alpha 分布（只打印非零桶）:");
    let buckets = [
        ("1-16", 1, 17),
        ("17-32", 17, 33),
        ("33-64", 33, 65),
        ("65-128", 65, 129),
        ("129-254", 129, 255),
        ("255", 255, 256),
    ];
    for (name, s, e) in buckets {
        let cnt: u64 = alpha_hist[s..e].iter().sum();
        if cnt > 0 {
            println!("  {name}: {cnt}");
        }
    }

    // --- 连通域（8 邻域，alpha >= 1）---
    let valid = |x: usize, y: usize| data[(y * w as usize + x) * 4 + 3] >= 1;
    let mut visited = vec![false; (w * h) as usize];
    let mut comps: Vec<(u64, u32, u32, u32, u32, u64, u64)> = Vec::new(); // area,x0,y0,x1,y1,sum_a,max_a
    let mut comp_pixels: Vec<Vec<(u32, u32)>> = Vec::new(); // 每个区域的像素坐标
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            if visited[idx] || !valid(x as usize, y as usize) {
                continue;
            }
            let mut q = VecDeque::new();
            q.push_back((x, y));
            visited[idx] = true;
            let mut area = 0u64;
            let (mut x0, mut y0, mut x1, mut y1) = (x, y, x, y);
            let (mut sum_a, mut max_a) = (0u64, 0u64);
            let mut pixels = Vec::new();
            while let Some((cx, cy)) = q.pop_front() {
                area += 1;
                pixels.push((cx, cy));
                x0 = x0.min(cx);
                y0 = y0.min(cy);
                x1 = x1.max(cx);
                y1 = y1.max(cy);
                let a = data[(cy as usize * w as usize + cx as usize) * 4 + 3] as u64;
                sum_a += a;
                max_a = max_a.max(a);
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = cx as i32 + dx;
                        let ny = cy as i32 + dy;
                        if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                            continue;
                        }
                        let nidx = (ny as usize * w as usize) + nx as usize;
                        if !visited[nidx] && valid(nx as usize, ny as usize) {
                            visited[nidx] = true;
                            q.push_back((nx as u32, ny as u32));
                        }
                    }
                }
            }
            comps.push((area, x0, y0, x1, y1, sum_a, max_a));
            comp_pixels.push(pixels);
        }
    }

    // comps 与 comp_pixels 必须同步排序，保证索引对应
    let mut comps: Vec<(u64, u32, u32, u32, u32, u64, u64, Vec<(u32, u32)>)> = comps
        .into_iter()
        .zip(comp_pixels)
        .map(|((a, x0, y0, x1, y1, sa, ma), px)| (a, x0, y0, x1, y1, sa, ma, px))
        .collect();
    comps.sort_by_key(|c| std::cmp::Reverse(c.0));
    println!("\n连通域总数（8 邻域, alpha>=1）: {}", comps.len());
    println!("{:>6} {:>6} {:>6} {:>6} {:>6} {:>6}  {:>8} {:>6}", "area", "x0", "y0", "x1", "y1", "w*h", "avg_a", "max_a");
    for c in comps.iter().take(60) {
        let (area, x0, y0, x1, y1, sum_a, max_a, _) = c;
        let bw = x1 - x0 + 1;
        let bh = y1 - y0 + 1;
        println!("{:>6} {:>6} {:>6} {:>6} {:>6} {:>6}  {:>8} {:>6}", area, x0, y0, x1, y1, format!("{bw}x{bh}"), sum_a / area, max_a);
    }
    if comps.len() > 60 {
        println!("... 其余 {} 个区域省略", comps.len() - 60);
    }

    // 面积分布统计
    let mut buckets_area = [0u64; 6]; // 1, 2-4, 5-16, 17-64, 65-256, >256
    for c in &comps {
        let a = c.0;
        let b = if a == 1 {
            0
        } else if a <= 4 {
            1
        } else if a <= 16 {
            2
        } else if a <= 64 {
            3
        } else if a <= 256 {
            4
        } else {
            5
        };
        buckets_area[b] += 1;
    }
    let names = ["1", "2-4", "5-16", "17-64", "65-256", ">256"];
    println!("\n面积分布（区域数）:");
    for (n, b) in names.iter().zip(buckets_area.iter()) {
        println!("  {n}: {b}");
    }

    // --- 真实 Alpha 值 TOP（按像素数排序）---
    let mut vals: Vec<(u8, u64)> = alpha_hist.iter().copied().enumerate().filter(|&(_, c)| c > 0).map(|(a, c)| (a as u8, c)).collect();
    vals.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
    println!("\n真实 Alpha 值 TOP 15:");
    for (a, c) in vals.iter().take(15) {
        println!("  alpha={a}: {c}");
    }

    // --- 小区域到主体（大区域）的距离 ---
    println!("\n小区域（area<=64）到最近主体像素的真实距离（切比雪夫, BFS）:");
    // 受保护像素掩码：属于大区域（area>256）的像素
    let mut protected = vec![false; (w * h) as usize];
    for c in &comps {
        if c.0 > 256 {
            for &(px, py) in &c.7 {
                protected[py as usize * w as usize + px as usize] = true;
            }
        }
    }
    // 对每个候选小区域做限界 BFS 求到受保护像素的真实距离
    for c in &comps {
        if c.0 > 64 {
            continue;
        }
        let (area, x0, y0, x1, y1, _, _, pixels) = c;
        let mut dist: Option<u32> = None;
        let mut seen = vec![false; (w * h) as usize];
        let mut frontier: VecDeque<(u32, u32)> = VecDeque::new();
        for &(x, y) in pixels {
            seen[y as usize * w as usize + x as usize] = true;
            frontier.push_back((x, y));
        }
        let mut d = 0u32;
        'outer: while !frontier.is_empty() && d < 600 {
            let mut next = VecDeque::new();
            while let Some((x, y)) = frontier.pop_front() {
                if protected[y as usize * w as usize + x as usize] {
                    dist = Some(d);
                    break 'outer;
                }
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                            continue;
                        }
                        let nidx = ny as usize * w as usize + nx as usize;
                        if !seen[nidx] {
                            seen[nidx] = true;
                            next.push_back((nx as u32, ny as u32));
                        }
                    }
                }
            }
            frontier = next;
            d += 1;
        }
        println!("  area={area} bbox=({x0},{y0})-({x1},{y1}) 真实距离={:?}", dist);
    }

    Ok(())
}
