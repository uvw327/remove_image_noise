//! 验收工具：检查清理前后的 PNG 是否满足数据保护约束。
//!
//! 用法：cargo run --release --example verify -- temp/a.png output/a.png
//!
//! 校验内容：
//!   1. 尺寸完全一致；
//!   2. 输出仍带 Alpha 通道；
//!   3. 所有改动只能是“Alpha > 0 变为 0”，且 RGB 原值不变。

use anyhow::{bail, Context, Result};
use image::GenericImageView;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let before_path = args.next().unwrap_or_else(|| "temp/a.png".into());
    let after_path = args.next().unwrap_or_else(|| "output/a.png".into());

    let before = image::open(&before_path)
        .with_context(|| format!("无法打开清理前的图片: {before_path}"))?;
    let after = image::open(&after_path)
        .with_context(|| format!("无法打开清理后的图片: {after_path}"))?;

    let (bw, bh) = before.dimensions();
    let (aw, ah) = after.dimensions();
    println!("清理前: {bw}x{bh}  {:?}", before.color());
    println!("清理后: {aw}x{ah}  {:?}", after.color());

    if (bw, bh) != (aw, ah) {
        bail!("尺寸不一致：清理前 {bw}x{bh}，清理后 {aw}x{ah}");
    }
    if !after.color().has_alpha() {
        bail!("清理后的图片没有 Alpha 通道");
    }

    let before = before.to_rgba8();
    let after = after.to_rgba8();
    let mut changed = 0u64;
    let mut cleared = 0u64;
    let mut rgb_changed = 0u64;
    let mut other = 0u64;

    for (bp, ap) in before.pixels().zip(after.pixels()) {
        if bp == ap {
            continue;
        }
        changed += 1;
        if bp.0[3] > 0 && ap.0[3] == 0 {
            cleared += 1;
            if bp.0[..3] != ap.0[..3] {
                rgb_changed += 1;
            }
        } else {
            other += 1;
        }
    }

    println!(
        "变更像素: {changed}（Alpha 清零 {cleared}，RGB 变化 {rgb_changed}，其他变化 {other}）"
    );

    if rgb_changed > 0 || other > 0 {
        bail!("存在违反数据保护约束的像素改动");
    }
    println!("校验通过：尺寸一致、Alpha 通道保留、仅 Alpha 被清零且 RGB 未改动。");
    Ok(())
}
