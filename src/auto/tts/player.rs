//! 音频播放 - 真实实现 (需要 audio feature)

use super::error::TtsError;
use colored::Colorize;
use rodio::{OutputStreamBuilder, Sink};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

// 嵌入音频文件
const HOUR_CHIME_AUDIO: &[u8] = include_bytes!("../../../voice/clock/Hour_Chime_from_Clock.mp3");

/// 音频播放请求
struct AudioRequest {
    path: PathBuf,
    /// 播放完成后通知的 channel
    done_tx: std::sync::mpsc::SyncSender<()>,
}

/// 全局音频播放队列
static AUDIO_QUEUE: OnceLock<Sender<AudioRequest>> = OnceLock::new();

/// 初始化音频播放器线程
fn get_audio_sender() -> &'static Sender<AudioRequest> {
    AUDIO_QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<AudioRequest>();

        // 启动单一播放线程，按顺序处理队列中的音频
        std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                play_audio_sync(&request.path);
                // 通知调用者播放完成
                let _ = request.done_tx.send(());
            }
        });

        tx
    })
}

/// 同步播放音频（内部使用，会阻塞直到播放完成）
fn play_audio_sync(path: &PathBuf) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            println!("{}", format!("  ✗ Failed to open: {}", e).red());
            return;
        }
    };

    let stream = match OutputStreamBuilder::open_default_stream() {
        Ok(s) => s,
        Err(e) => {
            println!("{}", format!("  ✗ No audio output: {}", e).red());
            return;
        }
    };

    let sink = Sink::connect_new(stream.mixer());

    let decoder = match rodio::Decoder::new(BufReader::new(file)) {
        Ok(d) => d,
        Err(e) => {
            println!("{}", format!("  ✗ Failed to decode: {}", e).red());
            return;
        }
    };

    sink.append(decoder);
    sink.sleep_until_end();
}

/// 播放音频文件（阻塞等待播放完成，但不阻塞其他线程）
pub fn play_audio(file_path: &PathBuf) -> Result<(), TtsError> {
    if !file_path.exists() {
        return Err(TtsError::PlayAudioFailed(format!(
            "Audio file not found: {}",
            file_path.display()
        )));
    }

    println!(
        "{}",
        format!("  🔊 Playing: {}", file_path.display()).green()
    );

    // 创建完成通知 channel
    let (done_tx, done_rx) = mpsc::sync_channel::<()>(0);

    let sender = get_audio_sender();
    sender
        .send(AudioRequest {
            path: file_path.clone(),
            done_tx,
        })
        .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to queue audio: {}", e)))?;

    // 等待播放完成
    done_rx
        .recv()
        .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to wait for playback: {}", e)))?;

    Ok(())
}

/// 获取音频目录 (~/.yo/voice/)
pub fn get_voice_dir() -> Result<PathBuf, TtsError> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| TtsError::PlayAudioFailed("Cannot find home directory".to_string()))?;

    let voice_dir = PathBuf::from(home).join(".yo").join("voice");
    fs::create_dir_all(&voice_dir)
        .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to create voice dir: {}", e)))?;

    // 确保 clock 目录和报时音频存在
    let clock_dir = voice_dir.join("clock");
    fs::create_dir_all(&clock_dir)
        .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to create clock dir: {}", e)))?;

    let chime_file = clock_dir.join("Hour_Chime_from_Clock.mp3");
    if !chime_file.exists() {
        fs::write(&chime_file, HOUR_CHIME_AUDIO)
            .map_err(|e| TtsError::PlayAudioFailed(format!("Failed to extract chime: {}", e)))?;
    }

    Ok(voice_dir)
}

/// 播放整点报时
pub fn play_hourly_chime() -> Result<(), TtsError> {
    let voice_dir = get_voice_dir()?;
    let chime_file = voice_dir.join("clock").join("Hour_Chime_from_Clock.mp3");

    if chime_file.exists() {
        println!("{}", "  🔔 Playing hour chime...".cyan());
        play_audio(&chime_file)
    } else {
        Err(TtsError::PlayAudioFailed(format!(
            "Hour chime not found: {}",
            chime_file.display()
        )))
    }
}
