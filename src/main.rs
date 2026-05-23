use anyhow::{Context, Result};
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use libheif_rs::{Chroma, ColorSpace, HeifContext};
use rayon::prelude::*;
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use walkdir::WalkDir;

/// 核心转换函数
/// - quality: 1-100 质量控制
/// - SamplingFactor::R_4_4_4 相当于 Python subsampling=0 (防止色彩溢出)
/// - 保留 EXIF (APP1) 和 ICC Profile (APP2)
fn convert_heic_to_jpg(heic_path: &Path, quality: u8) -> Result<()> {
    // 1. 读取 HEIC 文件
    let path_str = heic_path.to_str().context("路径包含无效字符")?;
    let ctx = HeifContext::read_from_file(path_str)?;
    let handle = ctx.primary_image_handle()?;

    let width = handle.width();
    let height = handle.height();

    // 2. 解码为 RGB24 (交错格式)
    let image = handle.decode(ColorSpace::Rgb(Chroma::Rgb), false)?;
    let planes = image.planes();
    let interleaved = planes
        .interleaved
        .ok_or_else(|| anyhow::anyhow!("无法获取交错像素数据"))?;
    let data = interleaved.data;

    let jpg_path = heic_path.with_extension("jpg");

    // 3. 配置 JPEG 编码器
    let mut encoder = Encoder::new_file(&jpg_path, quality)?;
    encoder.set_sampling_factor(SamplingFactor::R_4_4_4); 

    // --- 提取并注入 ICC Profile (Apple Display P3 等) ---
    let icc_profile = handle.color_profile_raw();
    if !icc_profile.is_empty() {
        let mut icc_payload = b"ICC_PROFILE\0\x01\x01".to_vec();
        icc_payload.extend_from_slice(icc_profile);
        encoder.add_app_segment(2, &icc_payload)?;
    }

    // --- 提取并注入 EXIF (拍摄时间、GPS、方向等) ---
    let exif_ids = handle.list_metadata_block_ids("Exif");
    if let Some(&id) = exif_ids.first() {
        if let Ok(exif_raw) = handle.metadata(id) {
            if exif_raw.len() > 4 {
                let mut exif_payload = b"Exif\0\0".to_vec();
                exif_payload.extend_from_slice(&exif_raw[4..]);
                encoder.add_app_segment(1, &exif_payload)?;
            }
        }
    }

    // 4. 执行编码并保存
    encoder.encode(data, width as u16, height as u16, ColorType::Rgb)?;

    println!(
        "✅ 已转换: {} -> {} (质量: {})",
        heic_path.file_name().unwrap_or_default().to_string_lossy(),
        jpg_path.file_name().unwrap_or_default().to_string_lossy(),
        quality
    );

    Ok(())
}

fn main() {
    // 1. 交互式获取路径
    println!("请输入包含 HEIC 文件的文件夹路径 (直接回车默认当前目录):");
    let mut input_dir = String::new();
    io::stdin().read_line(&mut input_dir).unwrap();
    let input_dir = input_dir.trim();
    
    let input_path = if input_dir.is_empty() {
        env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(input_dir)
    };

    // 2. 交互式获取质量 (模拟输入框，默认 100)
    print!("请输入输出 JPG 的质量 (1-100) [直接回车默认 100]: ");
    io::stdout().flush().unwrap(); // 确保提示语立刻显示在屏幕上
    
    let mut quality_input = String::new();
    io::stdin().read_line(&mut quality_input).unwrap();
    let quality_input = quality_input.trim();

    // 如果用户直接回车（输入为空），或者输入的不是数字，都默认使用 100
