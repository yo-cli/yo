// 测试整点报时功能（布谷鸟声音 + TTS 报时）
use std::path::PathBuf;

// 需要手动导入 tts 模块的代码
// 这里简化为直接调用编译后的库

fn main() {
    println!("=== Testing Hourly Chime Function ===\n");

    // 模拟当前时间为 11 点
    let hour = chrono::Local::now().hour();

    println!("🕐 Current hour: {}", hour);
    println!("Testing hourly chime...\n");

    // 1. 播放布谷鸟声音
    test_cuckoo();

    // 2. 播放报时（需要合成）
    test_time_announcement(hour);

    println!("\n✅ Test completed!");
}

fn test_cuckoo() {
    use std::fs::File;
    use std::io::BufReader;

    let file_path = PathBuf::from("voice/cuckoo.mp3");

    if !file_path.exists() {
        eprintln!("⚠ Cuckoo sound file not found");
        return;
    }

    println!("🐦 Playing cuckoo sound...");

    let file = File::open(&file_path).expect("Failed to open cuckoo file");
    let source = BufReader::new(file);

    let (_stream, stream_handle) = rodio::OutputStream::try_default()
        .expect("Failed to get audio output");

    let sink = rodio::Sink::try_new(&stream_handle)
        .expect("Failed to create audio sink");

    let decoder = rodio::Decoder::new(source)
        .expect("Failed to decode audio");

    sink.append(decoder);
    sink.sleep_until_end();

    println!("✓ Cuckoo sound played\n");
}

fn test_time_announcement(hour: u32) {
    println!("🔊 Time announcement: {}点整", hour);
    println!("(TTS synthesis would happen here in real implementation)");
}
