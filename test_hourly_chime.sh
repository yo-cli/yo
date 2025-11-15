#!/bin/bash
# 测试整点报时功能

echo "=== 测试整点报时功能 ==="
echo ""
echo "当前时间: $(date '+%Y-%m-%d %H:%M:%S')"
echo ""

# 使用编译好的二进制文件手动测试
# 这里我们通过创建一个临时的测试配置来测试

cat > /tmp/test_chime_config.json << 'EOF'
{
  "tasks": [
    {
      "name": "test_hourly_chime",
      "task_type": "hourly_chime",
      "start_time": "00:00",
      "end_time": "23:59",
      "interval_minutes": 60,
      "enabled": true,
      "tts_api_key": "7353882085|96Uy19kkSZEIrtxY8ospvBXP-AbdVOIp",
      "description": "测试整点报时"
    }
  ]
}
EOF

echo "✓ 测试配置已创建: /tmp/test_chime_config.json"
echo ""
echo "请手动执行以下步骤测试："
echo "1. 复制测试配置: cp /tmp/test_chime_config.json ~/.yo/auto_config.json"
echo "2. 运行调度器: yo run auto"
echo "3. 等待下一个整点或手动触发"
echo ""
echo "或者直接测试播放音频文件："
echo "cmd.exe /C start \"\" \"C:\\Users\\DEV\\.yo\\voice\\clock\\Hour_Chime_from_Clock.mp3\""
