@echo off
echo Testing embedded audio extraction...
echo.

REM 删除旧的音频目录
if exist "%USERPROFILE%\.yo\voice" (
    echo Removing old voice directory...
    rmdir /s /q "%USERPROFILE%\.yo\voice"
)

echo.
echo Running yo with embedded audio...
echo.

REM 运行程序（会自动提取音频文件）
timeout 5 target\release\yo.exe run auto

echo.
echo Checking extracted files...
dir /s "%USERPROFILE%\.yo\voice\clock\"

echo.
echo Done!
