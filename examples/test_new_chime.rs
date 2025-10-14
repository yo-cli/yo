// 测试新的时钟报时声音
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

fn main() {
    println!("🕐 Testing new hour chime sound...\n");

    let file_path = PathBuf::from("voice/clock/Hour_Chime_from_Clock.mp3");

    if !file_path.exists() {
        eprintln!("❌ File not found: {}", file_path.display());
        return;
    }

    println!("📁 File: {}", file_path.display());
    println!("🔔 Playing hour chime...\n");

    match play_audio(&file_path) {
        Ok(_) => println!("\n✅ Hour chime test passed!"),
        Err(e) => eprintln!("\n❌ Playback failed: {}", e),
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

    sink.append(decoder);
    sink.sleep_until_end();

    Ok(())
}
