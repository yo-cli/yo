// 测试布谷鸟声音播放
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

fn main() {
    let file_path = PathBuf::from("voice/cuckoo.mp3");

    println!("🐦 Testing cuckoo sound...");

    if !file_path.exists() {
        eprintln!("❌ File not found: {}", file_path.display());
        return;
    }

    match play_audio(&file_path) {
        Ok(_) => println!("✅ Cuckoo sound test passed!"),
        Err(e) => eprintln!("❌ Playback failed: {}", e),
    }
}

fn play_audio(file_path: &PathBuf) -> Result<(), String> {
    let file = File::open(file_path)
        .map_err(|e| format!("Failed to open file: {}", e))?;
    let source = BufReader::new(file);

    let (_stream, stream_handle) = rodio::OutputStream::try_default()
        .map_err(|e| format!("Failed to get audio output: {}", e))?;

    let sink = rodio::Sink::try_new(&stream_handle)
        .map_err(|e| format!("Failed to create audio sink: {}", e))?;

    let decoder = rodio::Decoder::new(source)
        .map_err(|e| format!("Failed to decode audio: {}", e))?;

    println!("🎵 Playing cuckoo sound...");
    sink.append(decoder);
    sink.sleep_until_end();
    println!("✓ Playback completed");

    Ok(())
}
