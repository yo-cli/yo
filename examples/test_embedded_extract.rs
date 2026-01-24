// 测试嵌入音频自动提取功能
// 注意：此示例需要在项目为 library crate 时才能运行
// 目前 yo 是纯 binary crate，无法从 examples 中引用内部模块

use std::path::PathBuf;

fn main() {
    println!("=== Testing Embedded Audio Extraction ===\n");

    // 获取配置目录
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("Cannot find home directory");

    let voice_dir = PathBuf::from(home).join(".yo").join("voice");
    let chime_file = voice_dir.join("clock").join("Hour_Chime_from_Clock.mp3");

    println!("Expected file location:");
    println!("  {}\n", chime_file.display());

    // 检查文件是否存在
    if chime_file.exists() {
        println!("✅ Audio file exists!");
        println!("   Location: {}", chime_file.display());

        let metadata = std::fs::metadata(&chime_file).unwrap();
        println!("   Size: {} bytes", metadata.len());
    } else {
        println!("⚠ Audio file not found at expected location.");
        println!("  Run 'yo run auto' once to trigger extraction.");
    }

    println!("\nTo test TTS functionality, run:");
    println!("  yo run ve");
}
