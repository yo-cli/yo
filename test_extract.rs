fn main() {
    println!("Testing embedded audio extraction...\n");
    
    // 直接使用嵌入的数据
    const HOUR_CHIME_AUDIO: &[u8] = include_bytes!("voice/clock/Hour_Chime_from_Clock.mp3");
    
    println!("Embedded audio size: {} bytes", HOUR_CHIME_AUDIO.len());
    println!("First 10 bytes: {:?}", &HOUR_CHIME_AUDIO[..10]);
    
    // 写入文件测试
    let test_file = std::path::PathBuf::from("C:/Users/DEV/.yo/voice/clock/test_extract.mp3");
    std::fs::create_dir_all(test_file.parent().unwrap()).unwrap();
    std::fs::write(&test_file, HOUR_CHIME_AUDIO).unwrap();
    
    println!("\n✅ Successfully extracted to: {}", test_file.display());
    println!("File size: {} bytes", std::fs::metadata(&test_file).unwrap().len());
}
