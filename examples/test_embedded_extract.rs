// 测试嵌入音频自动提取功能
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

    // 删除旧文件进行测试
    if chime_file.exists() {
        println!("Removing existing file for test...");
        std::fs::remove_file(&chime_file).expect("Failed to remove file");
    }

    println!("Creating TTS client (will trigger extraction)...\n");

    // 创建 TTS 客户端（使用任意 API key，不会真正调用 API）
    let api_key = "test|test".to_string();
    let client = yo::auto::tts::VolcengineTtsClient::new(api_key);

    // 调用 hourly_chime 会触发 get_voice_dir，从而提取嵌入的音频
    match client.hourly_chime(14) {
        Ok(_) => println!("\n✅ Test passed! Audio extracted and played successfully!"),
        Err(e) => {
            // 检查文件是否被提取
            if chime_file.exists() {
                println!("\n✅ Audio file extracted successfully!");
                println!("   Location: {}", chime_file.display());

                let metadata = std::fs::metadata(&chime_file).unwrap();
                println!("   Size: {} bytes", metadata.len());

                println!("\n⚠ Playback failed (expected in test): {}", e);
            } else {
                println!("\n❌ Test failed: {}", e);
            }
        }
    }
}
