//! 命令行参数定义。

use crate::clean::Mode;
use clap::Parser;
use std::path::PathBuf;

/// PNG 透明背景脏像素自动清理工具
#[derive(Debug, Parser)]
#[command(
    name = "remove_image_noise",
    version,
    about = "清理 AI 生成 PNG 透明背景中的零散脏像素，同时保护主体细节",
    long_about = "使用 8 邻域连通域分析识别透明背景中的孤立噪点。\n\
                  默认保守模式删除孤立小区域，并清理远离实心主体(Alpha>=128)的低 Alpha 杂色，\n\
                  保留 1-2 像素的正常抗锯齿边缘；不修改主体颜色，不模糊边缘，\n\
                  只把目标像素的 Alpha 置 0。"
)]
pub struct Cli {
    /// 输入 PNG 文件，或批量处理时的目录
    #[arg(short = 'i', long, value_name = "PATH")]
    pub input: PathBuf,

    /// 输出 PNG 文件，或批量处理时的输出目录（--dry-run 时可省略）
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// 清理模式：conservative 保守（默认）/ aggressive 激进
    #[arg(long, value_enum, default_value = "conservative", value_name = "MODE")]
    pub mode: Mode,

    /// 最大可清理连通区域面积（保守模式默认 8，激进模式默认 64）。
    /// 只统计 Alpha >= --alpha-threshold 的像素形成的连通域；若小噪点通过低 Alpha 像素与主体相连，
    /// 可提高 --alpha-threshold 切断连接后再用本参数过滤
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub max_area: Option<u32>,

    /// Alpha 检测阈值：alpha >= N 的像素视为有效像素（默认 1）。
    /// 提高该值可切断低 Alpha 像素构成的连接，使附着的小噪点成为独立连通域
    #[arg(
        long,
        value_name = "N",
        default_value_t = 1,
        value_parser = clap::value_parser!(u8).range(1..=255)
    )]
    pub alpha_threshold: u8,

    /// 低 Alpha 噪点过滤：仅清理最大 Alpha <= N 的区域，0 表示不限制（默认 0）
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub min_alpha: u8,

    /// 候选区域到主体的最小切比雪夫距离（默认 2，表示至少隔一个透明像素）
    #[arg(long, value_name = "N", default_value_t = 2)]
    pub min_distance: u32,

    /// 附着低 Alpha 杂色清理上限：Alpha <= N 且远离实心主体(Alpha>=128)的像素会被清除，0 表示关闭（默认 16）
    #[arg(long, value_name = "N", default_value_t = 16)]
    pub fringe_alpha: u8,

    /// 低 Alpha 杂色到实心主体的最小切比雪夫距离（默认 3）
    #[arg(long, value_name = "N", default_value_t = 3)]
    pub fringe_distance: u32,

    /// 窄颈分离后删除面积小于 N 的附着小凸起（即使与主体实心相连），0 表示关闭（默认 0）。
    /// 会改变主体轮廓，务必配合 --visualize 检查
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub prune_attached_area: u32,

    /// 只检测并输出结果，不写入任何图片文件
    #[arg(long)]
    pub dry_run: bool,

    /// 批量处理输入目录中的所有 PNG 文件
    #[arg(long)]
    pub batch: bool,

    /// 输出 JSON 检测报告；批量模式下为单个汇总报告文件
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,

    /// 输出噪点可视化图片（待清理像素标红）；批量模式下为输出目录
    #[arg(long, value_name = "PATH")]
    pub visualize: Option<PathBuf>,

    /// 允许输出文件覆盖输入文件（默认拒绝覆盖原图）
    #[arg(long)]
    pub force: bool,
}
