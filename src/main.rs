//! remove_image_noise：PNG 透明背景脏像素清理工具。
//!
//! 处理流程：读取 PNG -> 8 邻域连通域分析 -> 判定孤立噪点 -> 仅清零 Alpha -> 输出报告。
//! 具体算法见 clean.rs，连通域实现见 components.rs。

mod clean;
mod cli;
mod components;
mod report;
mod visualize;

use anyhow::{Context, Result, bail};
use clap::Parser;
use clean::{CleanOptions, CleanResult, PreserveReason, clean};
use cli::Cli;
use image::{GenericImageView, ImageFormat, RgbaImage};
use rayon::prelude::*;
use report::{BatchItem, Report};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            eprintln!("错误: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<bool> {
    let meta = fs::metadata(&cli.input)
        .with_context(|| format!("无法访问输入路径: {}", cli.input.display()))?;
    let opts = build_options(&cli);

    if meta.is_dir() {
        if !cli.batch {
            bail!("输入是目录，请添加 --batch 进行批量处理");
        }
        run_batch(&cli, &opts)
    } else {
        if cli.batch {
            bail!("--batch 只能用于目录输入");
        }
        run_single(&cli, &opts)
    }
}

/// 合并命令行参数与模式默认值
fn build_options(cli: &Cli) -> CleanOptions {
    CleanOptions {
        mode: cli.mode,
        max_area: cli.max_area.unwrap_or_else(|| cli.mode.default_max_area()),
        alpha_threshold: cli.alpha_threshold,
        min_alpha: cli.min_alpha,
        min_distance: cli.min_distance,
        fringe_alpha: cli.fringe_alpha,
        fringe_distance: cli.fringe_distance,
        prune_attached_area: cli.prune_attached_area,
    }
}

fn run_single(cli: &Cli, opts: &CleanOptions) -> Result<bool> {
    if !cli.dry_run && cli.output.is_none() {
        bail!("未指定输出路径：请使用 -o <PATH>，或使用 --dry-run 仅检测");
    }
    if let Some(output) = &cli.output {
        ensure_not_overwriting_input(&cli.input, output, cli.force)?;
    }
    if let Some(report) = &cli.report {
        ensure_not_overwriting_input(&cli.input, report, cli.force)?;
    }
    if let Some(visualize) = &cli.visualize {
        ensure_not_overwriting_input(&cli.input, visualize, cli.force)?;
    }

    let image = load_png(&cli.input)?;
    let (width, height) = image.dimensions();
    let result = clean(&image, opts);

    if let Some(output) = &cli.output
        && !cli.dry_run {
            save_png(&result.output, output)?;
        }
    if let Some(path) = &cli.visualize {
        let marked = visualize::render(&image, &result.output, &result.removed);
        save_png(&marked, path)?;
    }

    let report = Report::build(
        cli.input.display().to_string(),
        cli.output.as_ref().map(|p| p.display().to_string()),
        width,
        height,
        opts,
        &result,
        cli.dry_run,
    );
    if let Some(path) = &cli.report {
        write_json(path, &report)?;
    }

    print_single_report(&cli.input, width, height, opts, &result, cli);
    Ok(true)
}

fn run_batch(cli: &Cli, opts: &CleanOptions) -> Result<bool> {
    let output_dir = match (&cli.output, cli.dry_run) {
        (Some(dir), _) => Some(dir.clone()),
        (None, false) => bail!("批量模式未指定输出目录：请使用 -o <DIR>，或使用 --dry-run"),
        (None, true) => None,
    };

    if let Some(dir) = &output_dir {
        if !cli.force {
            ensure_not_same_dir(&cli.input, dir)?;
        }
        fs::create_dir_all(dir)
            .with_context(|| format!("无法创建输出目录: {}", dir.display()))?;
    }
    if let Some(dir) = &cli.visualize {
        if !cli.force {
            ensure_not_same_dir(&cli.input, dir)?;
        }
        fs::create_dir_all(dir)
            .with_context(|| format!("无法创建可视化输出目录: {}", dir.display()))?;
    }

    let mut files: Vec<PathBuf> = fs::read_dir(&cli.input)
        .with_context(|| format!("无法读取输入目录: {}", cli.input.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        })
        .collect();
    files.sort();

    if files.is_empty() {
        println!("目录中没有 PNG 文件: {}", cli.input.display());
        return Ok(true);
    }

    if let Some(report) = &cli.report
        && !cli.force
        && files.iter().any(|file| normalize(file) == normalize(report))
    {
        bail!(
            "报告路径与输入图片相同（{}），为避免覆盖原图已中止；如确需覆盖请添加 --force",
            report.display()
        );
    }

    let items: Vec<BatchItem> = files
        .par_iter()
        .map(|path| process_batch_file(path, cli, opts, output_dir.as_deref()))
        .collect();

    let success = items.iter().filter(|item| item.ok).count();
    let failed = items.len() - success;

    for item in &items {
        match (&item.report, &item.error) {
            (Some(report), _) => println!(
                "[OK] {}: 清理 {} 个区域 / {} 像素，保留 {} 个连通域",
                report.input,
                report.components_removed,
                report.pixels_removed,
                report.components_preserved
            ),
            (None, Some(error)) => println!("[FAIL] {}: {error}", item.input),
            _ => {}
        }
    }

    if let Some(path) = &cli.report {
        write_json(path, &items)?;
    }
    println!(
        "批量完成: 成功 {success} / {}，失败 {failed}",
        items.len()
    );
    if cli.dry_run {
        println!("（--dry-run：未生成清理后的图片）");
    }
    Ok(failed == 0)
}

fn process_batch_file(
    path: &Path,
    cli: &Cli,
    opts: &CleanOptions,
    output_dir: Option<&Path>,
) -> BatchItem {
    let input = path.display().to_string();
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let out_path = output_dir.map(|dir| dir.join(&file_name));
    let vis_path = cli.visualize.as_ref().map(|dir| {
        let stem = path
            .file_stem()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        dir.join(format!("{stem}.marked.png"))
    });

    let outcome = (|| -> Result<Report> {
        if let Some(out) = &out_path {
            ensure_not_overwriting_input(path, out, cli.force)?;
        }
        if let Some(vis) = &vis_path {
            ensure_not_overwriting_input(path, vis, cli.force)?;
        }
        let image = load_png(path)?;
        let (width, height) = image.dimensions();
        let result = clean(&image, opts);

        if let Some(out) = &out_path
            && !cli.dry_run {
                save_png(&result.output, out)?;
            }
        if let Some(vis) = &vis_path {
            let marked = visualize::render(&image, &result.output, &result.removed);
            save_png(&marked, vis)?;
        }

        Ok(Report::build(
            input.clone(),
            out_path.as_ref().map(|p| p.display().to_string()),
            width,
            height,
            opts,
            &result,
            cli.dry_run,
        ))
    })();

    match outcome {
        Ok(report) => BatchItem {
            input,
            output: out_path.map(|p| p.display().to_string()),
            ok: true,
            error: None,
            report: Some(report),
        },
        Err(err) => BatchItem {
            input,
            output: None,
            ok: false,
            error: Some(format!("{err:#}")),
            report: None,
        },
    }
}

fn load_png(path: &Path) -> Result<RgbaImage> {
    let reader = image::ImageReader::open(path)
        .with_context(|| format!("无法打开图片: {}", path.display()))?
        .with_guessed_format()
        .with_context(|| format!("无法识别图片格式: {}", path.display()))?;
    let decoded = reader
        .decode()
        .with_context(|| format!("解码图片失败: {}", path.display()))?;

    if !decoded.color().has_alpha() {
        bail!(
            "图片没有 Alpha 通道（色彩类型 {:?}），无法进行透明背景清理: {}",
            decoded.color(),
            path.display()
        );
    }
    if matches!(
        decoded.color(),
        image::ColorType::L16
            | image::ColorType::La16
            | image::ColorType::Rgb16
            | image::ColorType::Rgba16
            | image::ColorType::Rgb32F
            | image::ColorType::Rgba32F
    ) {
        bail!(
            "暂不支持 16 位或浮点 PNG（色彩类型 {:?}），避免转换时损失原始像素精度: {}",
            decoded.color(),
            path.display()
        );
    }
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        bail!("图片尺寸为 0: {}", path.display());
    }
    Ok(decoded.to_rgba8())
}

fn save_png(image: &RgbaImage, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建输出目录: {}", parent.display()))?;
        }
    image
        .save_with_format(path, ImageFormat::Png)
        .with_context(|| format!("写入 PNG 失败: {}", path.display()))
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建报告目录: {}", parent.display()))?;
        }
    let json = serde_json::to_string_pretty(value).context("序列化 JSON 报告失败")?;
    fs::write(path, json).with_context(|| format!("写入报告失败: {}", path.display()))
}

/// 规范化路径：优先使用 canonicalize；输出文件尚不存在时，改为规范化父目录再拼接文件名。
fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(canonical_parent) = fs::canonicalize(parent) {
            return canonical_parent.join(name);
        }
    path.to_path_buf()
}

fn ensure_not_overwriting_input(input: &Path, output: &Path, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    if normalize(input) == normalize(output) {
        bail!(
            "目标路径与输入路径相同（{}），为避免覆盖原图已中止；如确需覆盖请添加 --force",
            input.display()
        );
    }
    Ok(())
}

fn ensure_not_same_dir(input: &Path, output: &Path) -> Result<()> {
    if normalize(input) == normalize(output) {
        bail!("输出目录与输入目录相同，批量处理会覆盖原图；如确需覆盖请添加 --force");
    }
    Ok(())
}

fn print_single_report(
    input: &Path,
    width: u32,
    height: u32,
    opts: &CleanOptions,
    result: &CleanResult,
    cli: &Cli,
) {
    println!("文件: {}  ({}x{})", input.display(), width, height);
    let min_alpha = if opts.min_alpha > 0 {
        format!("  min-alpha={}", opts.min_alpha)
    } else {
        String::new()
    };
    println!(
        "模式: {}  max-area={}  alpha-threshold={}  min-distance={}{}",
        opts.mode.label(),
        opts.max_area,
        opts.alpha_threshold,
        opts.min_distance,
        min_alpha
    );
    println!("连通域: {} 个", result.components_total);
    println!(
        "清理: {} 个连通域 / {} 个像素",
        result.removed.len(),
        result.component_pixels_removed
    );
    if opts.fringe_alpha > 0 {
        println!(
            "边缘杂色: {} 个像素 / {} 个簇（fringe-alpha={}，fringe-distance={}）",
            result.fringe_pixels_removed,
            result.fringe.len(),
            opts.fringe_alpha,
            opts.fringe_distance
        );
    } else {
        println!("边缘杂色清理: 已关闭（--fringe-alpha 0）");
    }
    if opts.prune_attached_area > 0 {
        println!(
            "附着小凸起: {} 个像素 / {} 个区域（prune-attached-area={}）",
            result.prune_pixels_removed,
            result.pruned.len(),
            opts.prune_attached_area
        );
    }
    println!("合计删除: {} 个像素", result.pixels_removed);
    println!(
        "保留: {} 个区域（其中 {} 个主体区域）",
        result.preserved.len(),
        result
            .preserved
            .iter()
            .filter(|region| region.reason == PreserveReason::TooLarge)
            .count()
    );

    if result.removed.is_empty() {
        println!("未发现需要清理的孤立噪点。");
    } else {
        println!("待清理区域（按坐标排序）:");
        for region in result.removed.iter().take(20) {
            println!(
                "  - ({}, {}) {}x{}  area={}  alpha_max={}  distance={}",
                region.bbox.x0,
                region.bbox.y0,
                region.bbox.width(),
                region.bbox.height(),
                region.area,
                region.alpha_max,
                region.distance
            );
        }
        if result.removed.len() > 20 {
            println!(
                "  ... 其余 {} 个区域请查看 JSON 报告",
                result.removed.len() - 20
            );
        }
    }

    if !cli.dry_run
        && let Some(path) = &cli.output {
            println!("输出: {}", path.display());
        }
    if let Some(path) = &cli.visualize {
        println!("可视化: {}", path.display());
    }
    if let Some(path) = &cli.report {
        println!("报告: {}", path.display());
    }
    if cli.dry_run {
        println!("（--dry-run：未生成清理后的图片）");
    }
}
