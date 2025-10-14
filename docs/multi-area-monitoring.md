# Yo 多区域监控系统设计方案

## 📋 项目概述

基于 `yo` 项目扩展的多区域智能监控系统，支持：
- 🏠 多房间/区域监控（厨房、办公室、健身房、卧室等）
- 📹 无线摄像头接入
- 🎵 智能音箱语音提醒
- 🏋️ 健身动作识别与计数
- 📱 行为检测（刷手机、久坐、姿态等）

---

## 🎯 核心功能

### 1. 行为监控

#### 办公区域
- **刷手机检测**：超过设定时长自动提醒/锁屏
- **久坐提醒**：连续工作 2 小时提醒休息
- **坐姿监控**：检测不良坐姿，语音提醒纠正
- **疲劳检测**：眼睛闭合频率分析

#### 厨房区域
- **刷手机检测**：用餐/做饭时长时间看手机提醒
- **人员缺席检测**：灶台使用中人员离开超过 1 分钟报警
- **时间提醒**：饭点提醒

#### 健身区域
- **动作识别与计数**：
  - 俯卧撑（Push-ups）
  - 深蹲（Squats）
  - 平板支撑（Plank）
  - 其他可扩展动作
- **动作标准度评分**：关节角度、身体姿态、动作速度
- **实时语音指导**："手臂再弯一点"、"保持身体直线"
- **锻炼报告生成**：完成度、标准度、总分

#### 卧室区域
- **睡眠监控**：就寝时间提醒
- **睡前刷手机提醒**：超过就寝时间仍在使用手机

---

## 🏗️ 系统架构

### 方案 A：中央服务器架构（推荐）

```
┌─────────────────────────────────────────┐
│      中央服务器（主 PC）                 │
│      运行 yo monitor 主程序              │
│   - 多路视频流处理                       │
│   - AI 姿态/行为识别                     │
│   - 任务调度与提醒                       │
└───────────┬─────────────────────────────┘
            │ WiFi 局域网
    ┌───────┼───────┬──────────┐
    │       │       │          │
┌───▼───┐ ┌─▼────┐ ┌─▼──────┐ ┌─▼──────┐
│厨房   │ │办公室│ │健身房  │ │卧室    │
│摄像头 │ │摄像头│ │摄像头  │ │摄像头  │
│+ 音箱 │ │+ 音箱│ │+ 音箱  │ │+ 音箱  │
└───────┘ └──────┘ └────────┘ └────────┘
```

**优点**：
- 集中处理，算力充足
- 配置简单
- 易于调试

**缺点**：
- 网络带宽占用大
- 主机故障影响全系统

---

### 方案 B：分布式边缘计算

```
每个区域：
  摄像头 → 边缘设备(树莓派) → 本地AI → 事件上报 → 中央yo程序
```

**优点**：
- 减少带宽占用
- 响应更快
- 隐私更好（视频不传输）

**缺点**：
- 成本高
- 配置复杂

---

## 📹 摄像头方案

### 推荐型号对比

| 型号 | 价格 | 特点 | 适用场景 |
|------|------|------|----------|
| 小米智能摄像头 2K | ¥100/个 | WiFi、需破解RTSP | 预算有限 |
| TP-Link Tapo C200 | ¥200/个 | 原生RTSP、云台 | **推荐** |
| 海康威视 DS-2CD1021 | ¥500/个 | 企业级、POE供电 | 专业场景 |

### 接入协议

#### RTSP 推流（推荐）
```
rtsp://username:password@192.168.1.100:554/stream
```

**优点**：
- 标准协议
- 延迟低（100-300ms）
- 支持多客户端

**Rust 实现**：
```rust
use opencv::{prelude::*, videoio};

fn connect_rtsp_camera(url: &str) -> Result<VideoCapture> {
    let mut cam = videoio::VideoCapture::from_file(url, videoio::CAP_FFMPEG)?;
    if !cam.is_opened()? {
        return Err("Failed to open camera".into());
    }
    Ok(cam)
}
```

#### HTTP 抓图
```
http://192.168.1.100/snapshot.jpg
```

**优点**：简单
**缺点**：延迟高、不适合实时监控

---

## 🎵 音响设备集成

### 1. 智能音箱方案

#### 小米音箱（小爱同学）

**控制方式**：MiService API

```rust
async fn xiaomi_tts(ip: &str, text: &str) -> Result<()> {
    let client = reqwest::Client::new();
    client.post(format!("http://{}:8080/tts", ip))
        .json(&json!({
            "text": text,
            "volume": 50
        }))
        .send()
        .await?;
    Ok(())
}
```

**使用示例**：
```rust
xiaomi_tts("192.168.1.101", "检测到长时间刷手机，该吃饭了！").await?;
```

#### 天猫精灵

**控制方式**：阿里云 IoT 平台

```rust
async fn tmall_tts(device_id: &str, text: &str) -> Result<()> {
    let client = reqwest::Client::new();
    client.post("https://api.tmall.com/smart/tts")
        .header("Authorization", format!("Bearer {}", TOKEN))
        .json(&json!({
            "deviceId": device_id,
            "text": text
        }))
        .send()
        .await?;
    Ok(())
}
```

---

### 2. 树莓派 + 音箱方案（最灵活）

**每个区域部署**：树莓派 Zero 2W + USB 声卡 + 音箱

**树莓派端服务**（Python）：
```python
from flask import Flask, request
import subprocess

app = Flask(__name__)

@app.route('/speak', methods=['POST'])
def speak():
    text = request.json['text']
    subprocess.run(['espeak', '-v', 'zh', text])
    return 'OK'

@app.route('/play', methods=['POST'])
def play():
    sound = request.json['sound']
    subprocess.run(['aplay', f'/sounds/{sound}.wav'])
    return 'OK'

if __name__ == '__main__':
    app.run(host='0.0.0.0', port=8080)
```

**yo 程序调用**：
```rust
async fn notify_area(area: &str, message: &str) -> Result<()> {
    let speaker_ip = get_speaker_ip(area);

    reqwest::Client::new()
        .post(format!("http://{}:8080/speak", speaker_ip))
        .json(&json!({ "text": message }))
        .send()
        .await?;

    Ok(())
}
```

---

## 🤖 AI 技术方案

### 姿态检测：MediaPipe Pose

**为什么选择 MediaPipe 而不是 YOLO？**

| 对比项 | MediaPipe Pose | YOLO | YOLO-Pose |
|--------|---------------|------|-----------|
| 检测精度 | 33 个关键点 | 只有边界框 | 17 个关键点 |
| 速度 | 60fps (CPU) | 30fps (GPU) | 20fps (GPU) |
| 资源占用 | 低 | 高 | 中 |
| 适用场景 | 单人姿态 | 目标检测 | 多人姿态 |

**结论**：姿态检测优先选 MediaPipe，多人场景可结合 YOLO

---

### 健身动作识别算法

#### 俯卧撑识别

```python
def detect_pushup(landmarks, state, count):
    # 1. 计算肘部角度
    elbow_angle = calculate_angle(
        landmarks['shoulder'],
        landmarks['elbow'],
        landmarks['wrist']
    )

    # 2. 计算身体直线度
    body_angle = calculate_angle(
        landmarks['shoulder'],
        landmarks['hip'],
        landmarks['ankle']
    )

    # 3. 状态机
    if state == 'up' and elbow_angle < 90:
        state = 'down'
        speak("下")
    elif state == 'down' and elbow_angle > 160:
        state = 'up'
        count += 1
        speak(f"起！完成 {count} 个")

    # 4. 评分
    score = 100
    if elbow_angle > 100:
        score -= 20
        feedback = "手臂弯曲不够"
    if body_angle < 160:
        score -= 30
        feedback = "身体不够直，腰部下塌"

    return count, state, score, feedback
```

#### 深蹲识别

```python
def detect_squat(landmarks, state, count):
    # 计算膝盖角度
    knee_angle = calculate_angle(
        landmarks['hip'],
        landmarks['knee'],
        landmarks['ankle']
    )

    # 状态判断
    if state == 'up' and knee_angle < 100:
        state = 'down'
    elif state == 'down' and knee_angle > 160:
        state = 'up'
        count += 1

    # 评分
    score = 100
    if knee_angle < 90:
        score -= 20  # 蹲太低
    if back_angle < 150:
        score -= 25  # 弯腰了

    return count, state, score
```

---

### 锻炼评分系统

```python
class WorkoutSession:
    def __init__(self):
        self.exercises = []

    def add_exercise(self, name, count, target, scores):
        """
        name: 动作名称
        count: 完成次数
        target: 目标次数
        scores: 每次动作评分列表
        """
        self.exercises.append({
            'name': name,
            'count': count,
            'target': target,
            'scores': scores,
            'avg_score': sum(scores) / len(scores)
        })

    def calculate_final_score(self):
        total = 0
        for ex in self.exercises:
            # 完成度 40% + 标准度 60%
            completion = min(ex['count'] / ex['target'], 1.0) * 40
            quality = ex['avg_score'] * 0.6
            ex['final_score'] = completion + quality
            total += ex['final_score']

        return total / len(self.exercises)

    def generate_report(self):
        return f"""
        ═══════════════════════════════════
        🏋️ 锻炼报告
        ═══════════════════════════════════
        时间：{datetime.now()}
        总分：{self.calculate_final_score():.1f}/100

        {''.join([
            f"【{ex['name']}】\n"
            f"  完成：{ex['count']}/{ex['target']} 个\n"
            f"  标准度：{ex['avg_score']:.1f}/100\n"
            f"  得分：{ex['final_score']:.1f}/100\n\n"
            for ex in self.exercises
        ])}
        """
```

---

## 📝 配置文件设计

### `~/.yo/monitor_config.json`

```json
{
  "areas": [
    {
      "name": "厨房",
      "camera": {
        "type": "rtsp",
        "url": "rtsp://admin:password@192.168.1.100:554/stream",
        "resolution": "1280x720",
        "fps": 15
      },
      "speaker": {
        "type": "xiaomi",
        "ip": "192.168.1.101",
        "name": "小爱音箱"
      },
      "monitors": [
        {
          "type": "phone_detection",
          "duration_threshold": 300,
          "message": "放下手机，该吃饭了！"
        },
        {
          "type": "person_absent",
          "duration_threshold": 60,
          "message": "检测到长时间无人，请检查灶台"
        }
      ]
    },
    {
      "name": "办公室",
      "camera": {
        "type": "rtsp",
        "url": "rtsp://admin:password@192.168.1.102:554/stream"
      },
      "speaker": {
        "type": "tmall",
        "ip": "192.168.1.103"
      },
      "monitors": [
        {
          "type": "posture",
          "check_interval": 300,
          "message": "坐姿不正确，请调整"
        },
        {
          "type": "fatigue",
          "check_interval": 7200,
          "message": "已工作2小时，该休息了"
        },
        {
          "type": "phone_detection",
          "duration_threshold": 600,
          "action": "lock_screen"
        }
      ]
    },
    {
      "name": "健身房",
      "camera": {
        "type": "rtsp",
        "url": "rtsp://admin:password@192.168.1.104:554/stream"
      },
      "speaker": {
        "type": "bluetooth",
        "mac": "AA:BB:CC:DD:EE:FF"
      },
      "monitors": [
        {
          "type": "workout",
          "exercises": [
            {
              "name": "俯卧撑",
              "target": 20,
              "voice_feedback": true
            },
            {
              "name": "深蹲",
              "target": 30,
              "voice_feedback": true
            },
            {
              "name": "平板支撑",
              "target_duration": 60,
              "voice_feedback": true
            }
          ]
        }
      ]
    },
    {
      "name": "卧室",
      "camera": {
        "type": "rtsp",
        "url": "rtsp://admin:password@192.168.1.105:554/stream"
      },
      "speaker": {
        "type": "xiaomi",
        "ip": "192.168.1.106"
      },
      "monitors": [
        {
          "type": "sleep_monitor",
          "bedtime": "23:00",
          "message": "该睡觉了，早点休息"
        },
        {
          "type": "phone_detection",
          "after_bedtime": true,
          "duration_threshold": 300,
          "message": "别玩手机了，明天还要工作"
        }
      ]
    }
  ],
  "global_settings": {
    "privacy_hours": ["00:00-06:00"],
    "disable_areas": [],
    "log_level": "info",
    "data_retention_days": 30
  }
}
```

---

## 💻 命令行接口设计

### 新增命令

```bash
# 配置管理
yo monitor setup              # 初始化配置，扫描设备
yo monitor config             # 编辑配置文件
yo monitor test camera        # 测试摄像头连接
yo monitor test speaker       # 测试音箱

# 监控控制
yo monitor start              # 启动全部区域监控
yo monitor start kitchen      # 只启动厨房监控
yo monitor stop               # 停止监控
yo monitor restart            # 重启监控
yo monitor status             # 查看各区域状态

# 健身模式
yo workout start              # 开始锻炼（交互式选择动作）
yo workout start pushup 20    # 直接开始 20 个俯卧撑
yo workout report             # 查看锻炼报告
yo workout history            # 历史记录

# 日志与调试
yo monitor logs               # 查看实时日志
yo monitor logs kitchen       # 查看厨房日志
yo monitor stats              # 统计信息
yo monitor debug              # 调试模式（显示视频流）
```

---

### 状态显示界面

```bash
$ yo monitor status

╔═══════════════════════════════════════════════════════════════╗
║  🏠 Multi-Area Monitoring System                             ║
║  Started: 2025-10-13 08:00:00 | Uptime: 14h 32m             ║
╠═══════════════════════════════════════════════════════════════╣
║                                                               ║
║  【厨房】 ✓ ONLINE                                            ║
║    📹 Camera: rtsp://192.168.1.100:554 [1280x720@15fps]     ║
║    🔊 Speaker: 小爱音箱 (192.168.1.101)                       ║
║    👤 Status: 有人 (检测到 1 人)                              ║
║    📱 Phone: 未检测到                                         ║
║    ⏱️  Duration: 15分钟                                       ║
║                                                               ║
╠───────────────────────────────────────────────────────────────╣
║                                                               ║
║  【办公室】 ✓ ONLINE                                          ║
║    📹 Camera: rtsp://192.168.1.102:554 [1280x720@15fps]     ║
║    🔊 Speaker: 天猫精灵 (192.168.1.103)                       ║
║    👤 Status: 工作中 (持续时间: 2小时 15分钟)                 ║
║    💺 Posture: 良好                                           ║
║    📱 Phone: 检测到 (5分钟) ⚠️                                ║
║    📊 Alerts: 3 次姿态提醒                                    ║
║                                                               ║
╠───────────────────────────────────────────────────────────────╣
║                                                               ║
║  【健身房】 ✗ OFFLINE                                         ║
║    📹 Camera: 连接失败                                        ║
║    🔧 Action: 正在重试... (3/10)                             ║
║                                                               ║
╠───────────────────────────────────────────────────────────────╣
║                                                               ║
║  【卧室】 ⏸️  PAUSED (隐私时段)                               ║
║    📹 Camera: 已暂停                                          ║
║    ⏰ Resume at: 06:00                                        ║
║                                                               ║
╚═══════════════════════════════════════════════════════════════╝

ℹ️  Config: ~/.yo/monitor_config.json
ℹ️  Logs: ~/.yo/logs/monitor_2025-10-13.log
⚡ CPU: 25% | RAM: 1.2GB | Network: ↓ 2.3 MB/s
```

---

## 🚀 实现路线图

### Phase 1：单区域原型（1 周）
- [ ] 接入单个 USB/RTSP 摄像头
- [ ] 实现刷手机行为检测（MediaPipe + 手部检测）
- [ ] PC 本地音频/弹窗提醒
- [ ] 基础命令行界面

**技术栈**：
- Rust (主程序)
- Python (MediaPipe 姿态检测)
- FFI 桥接

---

### Phase 2：无线扩展（1-2 周）
- [ ] 支持多个 RTSP 摄像头
- [ ] 集成智能音箱（小米/天猫精灵）
- [ ] 实现远程语音提醒
- [ ] 配置文件系统

**新增功能**：
- 久坐提醒
- 坐姿监控
- 多摄像头切换

---

### Phase 3：多区域（2-3 周）
- [ ] 多摄像头同时处理（多线程）
- [ ] 区域配置与管理
- [ ] 多音箱智能选择
- [ ] 状态监控面板

**新增功能**：
- 厨房安全监控
- 卧室睡眠监控
- 实时状态显示

---

### Phase 4：健身功能（2-3 周）
- [ ] 俯卧撑识别与计数
- [ ] 深蹲识别与计数
- [ ] 平板支撑计时
- [ ] 动作标准度评分
- [ ] 实时语音指导
- [ ] 锻炼报告生成

**新增功能**：
- 多种动作支持
- 历史数据统计
- 进步曲线图表

---

### Phase 5：完善与优化（持续）
- [ ] 性能优化（减少 CPU 占用）
- [ ] UI 美化（可选 Web 界面）
- [ ] 移动端支持（APP 查看状态）
- [ ] 云同步（可选）
- [ ] 更多动作识别
- [ ] AI 智能建议

---

## 💰 成本预算

### 方案 A：经济型（总计约 1500 元）

| 项目 | 数量 | 单价 | 小计 |
|------|------|------|------|
| TP-Link Tapo C200 摄像头 | 4 | 200 | 800 |
| 小米音箱 Play | 4 | 100 | 400 |
| USB 声卡（可选） | 2 | 50 | 100 |
| 网线、支架等配件 | - | - | 200 |

**总计**：¥1500

---

### 方案 B：专业型（总计约 5000 元）

| 项目 | 数量 | 单价 | 小计 |
|------|------|------|------|
| 海康威视 POE 摄像头 | 4 | 500 | 2000 |
| 树莓派 Zero 2W | 4 | 250 | 1000 |
| USB 声卡 + 音箱 | 4 | 200 | 800 |
| POE 交换机 | 1 | 300 | 300 |
| 网线、支架等配件 | - | - | 500 |
| SD 卡、电源等 | - | - | 400 |

**总计**：¥5000

---

## 🔒 隐私与安全

### 数据处理原则
- ✅ **本地处理**：所有视频流在本地处理，不上传云端
- ✅ **不保存视频**：只保存统计数据和事件日志
- ✅ **可选关闭**：任何时候可以物理/软件关闭摄像头
- ✅ **隐私时段**：可设置自动暂停时间（如深夜、周末）
- ✅ **数据加密**：本地数据库加密存储

### 安全措施
- 摄像头独立 VLAN（与外网隔离）
- 音箱不上传数据
- 定期清理日志（默认保留 30 天）
- 访客模式（暂停所有监控）

---

## 📚 技术栈总结

### 后端（Rust）
```toml
[dependencies]
tokio = { version = "1.0", features = ["full"] }
opencv = "0.88"
reqwest = { version = "0.11", features = ["json"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
chrono = "0.4"
colored = "2.0"
anyhow = "1.0"
```

### AI 处理（Python）
```
mediapipe==0.10.8
opencv-python==4.8.1
numpy==1.24.3
```

### 数据存储
- SQLite（本地数据库）
- JSON（配置文件）

---

## 🎯 下一步行动

### 优先级排序

**P0 - 立即开始**：
1. 购买一个 TP-Link Tapo C200 测试（¥200）
2. 实现单摄像头 + 刷手机检测
3. PC 本地提醒

**P1 - 第二阶段**：
1. 集成小米音箱（如果已有）
2. 多区域配置系统
3. 办公室久坐提醒

**P2 - 第三阶段**：
1. 健身动作识别
2. 完整报告系统
3. Web 管理界面

---

## 📞 讨论记录

### 关键决策
- ✅ 使用 MediaPipe 而非 YOLO 进行姿态检测（更适合单人场景）
- ✅ 采用中央服务器架构（实现更简单）
- ✅ RTSP 摄像头 + 智能音箱方案（性价比高）
- ✅ Rust 主程序 + Python AI 处理（发挥各自优势）

### 待确认事项
- [ ] 摄像头具体型号选择
- [ ] 音箱方案（智能音箱 vs 树莓派）
- [ ] 首先实现的监控区域（建议从办公室开始）

---

## 📝 更新日志

- **2025-10-13**：初始设计文档完成
  - 完成多区域架构设计
  - 完成技术方案选型
  - 完成成本预算
  - 完成实现路线图

---

## 🔗 相关文档

- [MediaPipe Pose 官方文档](https://google.github.io/mediapipe/solutions/pose.html)
- [OpenCV Rust 文档](https://docs.rs/opencv/latest/opencv/)
- [RTSP 协议说明](https://en.wikipedia.org/wiki/Real_Time_Streaming_Protocol)
- [TP-Link Tapo 摄像头 API](https://github.com/JurajNyiri/pytapo)

---

**文档维护者**：Claude + 用户
**最后更新**：2025-10-13
**版本**：v1.0
