#!/bin/bash
echo "Testing TTS API call..."
./target/release/yo.exe run auto &
PID=$!
echo "Started yo with PID: $PID"
sleep 5
echo "Stopping yo..."
kill $PID 2>/dev/null
sleep 1
echo "Checking for generated audio file..."
ls -lh voice/last_tts.mp3 2>/dev/null || echo "No audio file generated yet"
file voice/last_tts.mp3 2>/dev/null || echo "File command not available"
