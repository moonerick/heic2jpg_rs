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
    encoder.set_sampling_factor(SamplingFactor::R_4_4_4); // 对应 Python 的 subsampling: 0

    // --- 提取并注入 ICC Profile (Apple Display P3 等) ---
    // ICC Profile 在 JPEG 中保存在 APP2 数据段
    let icc_profile = handle.color_profile_raw();
    if !icc_profile.is_empty() {
        // 构建 JPEG APP2 的 ICC Chunk Header: "ICC_PROFILE\0" + sequence(1) + total(1)
        let mut icc_payload = b"ICC_PROFILE\0\x01\x01".to_vec();
        icc_payload.extend_from_slice(icc_profile);
        encoder.add_app_segment(2, &icc_payload)?;
    }

    // --- 提取并注入 EXIF (拍摄时间、GPS、方向等) ---
    // EXIF 在 JPEG 中保存在 APP1 数据段，格式需带有 "Exif\0\0" 头部
    let exif_ids = handle.list_metadata_block_ids("Exif");
    if let Some(&id) = exif_ids.first() {
        if let Ok(exif_raw) = handle.metadata(id) {
            // libheif 提取的 EXIF 数据通常前 4 个字节是 offset/size (HEIF 标准)
            // 真正的 TIFF EXIF 数据从第 4 字节后开始
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
    // 解析命令行参数
    let args: Vec<String> = env::args().collect();
    let input_path = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    let quality_val: u8 = if args.len() > 2 {
        args[2].parse().unwrap_or(95)
    } else {
        95
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

    println!("--- 任务开始 ---");
    println!("找到文件: {} 个", heic_files.len());
    println!("输出目录: {}", input_path.display());
    println!("使用线程: {} 个 | 设定质量: {}", num_procs, quality_val);
    println!("----------------");

    // 原子计数器，用于统计成功数量
    let success_count = AtomicUsize::new(0);

    // 使用 Rayon 并行迭代处理 (相当于 Python 的 Pool.map)
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

    // 解决 Windows 下控制台一闪而过的问题
    println!("\n任务结束。按回车键退出...");
    io::stdout().flush().unwrap();
    let _ = io::stdin().read_line(&mut String::new());
}
