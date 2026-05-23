use anyhow::{Context, Result};
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use libheif_rs::{ColorSpace, HeifContext, ItemId, LibHeif, RgbChroma};
use rayon::prelude::*;
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use walkdir::WalkDir;

/// 核心转换函数
fn convert_heic_to_jpg(heic_path: &Path, quality: u8) -> Result<()> {
    // 1. 读取 HEIC 文件
    let path_str = heic_path.to_str().context("路径包含无效字符")?;
    let ctx = HeifContext::read_from_file(path_str)?;
    let handle = ctx.primary_image_handle()?;

    let width = handle.width();
    let height = handle.height();

    // 2. 解码为 RGB24 (交错格式)
    // 适配新版 1.1.0+: 必须通过 LibHeif 实例来进行解码，且使用 RgbChroma 强类型
    let lib_heif = LibHeif::new();
    let image = lib_heif.decode(&handle, ColorSpace::Rgb(RgbChroma::Rgb), None)
        .context("解码 HEIC 图像失败")?;
        
    let planes = image.planes();
    let interleaved = planes
        .interleaved
        .ok_or_else(|| anyhow::anyhow!("无法获取交错像素数据"))?;
    let data = interleaved.data;

    let jpg_path = heic_path.with_extension("jpg");

    // 3. 配置 JPEG 编码器
    let mut encoder = Encoder::new_file(&jpg_path, quality)?;
    encoder.set_sampling_factor(SamplingFactor::R_4_4_4); 

    // --- 提取并注入 ICC Profile ---
    // 适配新版: data 变成了一个公开字段，而不是方法
    if let Some(icc_profile) = handle.color_profile_raw() {
        let icc_data = &icc_profile.data;
        if !icc_data.is_empty() {
            let mut icc_payload = b"ICC_PROFILE\0\x01\x01".to_vec();
            icc_payload.extend_from_slice(icc_data);
            encoder.add_app_segment(2, &icc_payload)?;
        }
    }

    // --- 提取并注入 EXIF ---
    // 适配新版: 明确指定 ItemId 类型，且不再对 b"Exif" 使用星号解引用
    let mut item_ids: Vec<ItemId> = vec![Default::default(); 1]; 
    let count = handle.metadata_block_ids(&mut item_ids, b"Exif");
    if count > 0 {
        if let Ok(exif_raw) = handle.metadata(item_ids[0]) {
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

    // 2. 交互式获取质量
    print!("请输入输出 JPG 的质量 (1-100) [直接回车默认 100]: ");
    io::stdout().flush().unwrap();
    
    let mut quality_input = String::new();
    io::stdin().read_line(&mut quality_input).unwrap();
    let quality_input = quality_input.trim();

    // 如果用户直接回车（输入为空），或者输入的不是数字，都默认使用 100
    let quality_val: u8 = if quality_input.is_empty() {
        100
    } else {
        quality_input.parse().unwrap_or(100)
    };

    // 递归查找 HEIC 文件
    let mut heic_files = Vec::new();
    for entry in WalkDir::new(&input_path).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif") {
                    heic_files.push(path.to_path_buf());
                }
            }
        }
    }

    if heic_files.is_empty() {
        println!("在 {} 中未找到 HEIC 文件。", input_path.display());
        println!("\n按回车键退出...");
        let _ = io::stdin().read_line(&mut String::new());
        return;
    }

    // 计算进程数：取核心数的 75%
    let logical_cpus = num_cpus::get();
    let num_procs = std::cmp::max((logical_cpus as f32 * 0.75) as usize, 1);

    // 配置 Rayon 全局线程池
    rayon::ThreadPoolBuilder::new()
        .num_threads(num_procs)
        .build_global()
        .unwrap();

    println!("\n--- 任务开始 ---");
    println!("找到文件: {} 个", heic_files.len());
    println!("输出目录: {}", input_path.display());
    println!("使用线程: {} 个 | 设定质量: {}%", num_procs, quality_val);
    println!("----------------\n");

    // 原子计数器，用于统计成功数量
    let success_count = AtomicUsize::new(0);

    // 并行处理
    heic_files.par_iter().for_each(|file| {
        match convert_heic_to_jpg(file, quality_val) {
            Ok(_) => {
                success_count.fetch_add(1, Ordering::SeqCst);
            }
            Err(e) => {
                println!(
                    "❌ 失败 {}: {}",
                    file.file_name().unwrap_or_default().to_string_lossy(),
                    e
                );
            }
        }
    });

    let successes = success_count.load(Ordering::SeqCst);
    println!("\n--- 统计结果 ---");
    println!(
        "总计: {} | 成功: {} | 失败: {}",
        heic_files.len(),
        successes,
        heic_files.len() - successes
    );

    println!("\n任务结束。按回车键退出...");
    io::stdout().flush().unwrap();
    let _ = io::stdin().read_line(&mut String::new());
}
