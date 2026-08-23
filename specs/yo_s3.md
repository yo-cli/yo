# yo-s3 产品需求规格

> 本文档面向 AI 开发助手，描述 yo-s3 的产品定位、核心场景和设计决策。

---

## 1. 产品定位

**yo-s3 是一个「按预算可控地消耗指定金额 AWS 成本」的工具。**

目标用户是需要在自己拥有（或已获授权）的 AWS 账号里，**主动产生一笔可控、可预估、花够即停的真实账单**的开发者。典型用途：

- **消耗到期的 credits / 承诺消费额度**（EDP / promotional credits，use-it-or-lose-it）
- **验证账单告警与成本护栏**（CloudWatch Billing Alarm、Budgets、Cost Anomaly Detection 是否按预期触发）
- **验证成本分摊标签**（Cost Allocation Tags 是否正确归属）

核心理念：**给一个金额，它就精确烧到那个数然后停下，全程告诉你烧了多少、还差多少。**

它的手段是「以可控随机速率向 S3 持续写入大对象、并通过跨区复制放大流量成本」，但**写入吞吐只是手段，"花够指定金额"才是目的**——终止条件是金额，不是数据量。

> ⚠️ **使用边界**：本工具仅用于操作**你自己拥有或已获明确授权的 AWS 账号**。它打的是你自己的桶、花的是你自己的预算，不针对任何第三方资源。所有护栏（预算硬上限、启动确认、dry-run）都是为防止手滑多花钱，不得移除。

---

## 2. 与家族其他工具的关系

yo-s3 是**独立 binary**，与 yo-s5（代理类）无任何功能重叠，唯一共性是沿用家族 CLI 风格：clap derive 子命令、colored 输出符号（✓/✗/⚠/ℹ/📊）、inquire 交互补齐、纯 Rust 无 OpenSSL（aws-sdk 走 rustls）。因此单独成 binary，不并入任何现有工具。

---

## 3. 成本模型：为什么是「跨区复制」当引擎

要在**合理时间内**烧到可观金额，必须选对成本项。三项主要成本的量级（S3 Standard，us-east-1 基准单价，写入侧视角）：

| 成本项 | 单价 | 写 1 TiB 立即产生 | 烧 $500 需要 | 特性 |
|--------|------|------------------|-------------|------|
| PUT / multipart 请求费 | $0.005 / 1000 req | ~$0.02 | 写约 25 PB | **可忽略** |
| 存储费 | $0.023 / GB·月 | ~$23.5 / **整月** | 存 21 TiB 满一个月 | **极慢，按时间发酵，运行期无法精确控停** |
| **跨区复制流量费** | **$0.02 / GB** | **~$20，写完即产生** | **写约 25 TiB** | **即时、随写入线性、可精确控停** ← 主引擎 |

**结论**：光靠「写数据 + 留着」烧钱极慢（存储费按月发酵）。唯一能靠「写」这个动作**立即、大额、可精确控制到某一刻停下**的，是**跨区复制（Cross-Region Replication, CRR）产生的跨区流量费**。故 CRR 是 yo-s3 的**默认核心引擎**，不是可选项。

**预算口径（budget 统计哪些）**：默认只统计**即时成本** = 写入请求费 + 跨区流量费 + 目标端复制请求费。这三项随写入实时产生、可精确"花够即停"。**存储费不计入 budget 的停止判据**（它在工具停止后仍按时间累积，运行期无法精确控停），但会按保留期在成本预估页**单独估算展示**，让用户心里有数。

---

## 4. 核心场景

### 4.1 `yo-s3`（默认动作 = 按预算烧钱）

**需求：** 用户想在自己账号里花掉指定金额。

**行为：**
- **必填只有两项**，命令行没给就交互询问并给建议默认值（回车即接受）：
  - `--budget <金额>`：要烧掉多少美元（如 `500`）
  - `--bucket <名>`：源桶名
- 其余全部有缺省，无需出现在命令行：对象 1 TiB、part 256 MiB、内存池 2 GiB、并发 1 对象 × 4 part、速率区间 200–500 MiB/s、每 30s 重采样一次速率、region 取 EC2 实例元数据 / 默认 profile
- 启动流程（每步 ✓/✗/⚠ 呈现）：
  1. 解析 AWS 凭据链（EC2 IAM Role / 环境变量 / `~/.aws`），打印**当前认证身份**（`sts get-caller-identity`）
  2. 配置合法性校验（part ∈ [5 MiB, 5 GiB]、单对象 ≤ 10 000 part、≤ 5 TiB、池 ≥ 2×part）
  3. `HeadBucket` 连通性；**检测桶是否开启版本控制**（CRR 要求开，未开则在 setup-crr 里帮开）
  4. **检测跨区复制是否已配置**：已配 → 显示目标桶 + 目标 region；未配 → 提示先跑 `yo-s3 setup-crr`（或交互确认当场帮配）
  5. **成本预估页**：按 region 真实单价，列出「预计写入总量、跨区流量费、请求费、（附带）保留期内存储费估算」，合计对齐到 `--budget`
  6. `--yes` 或交互确认后开跑
- 运行中每 `--report-interval`（默认 10s）打印一行：瞬时/累计吞吐、实际 vs 目标速率、**已烧 $X / 目标 $Y、预计剩余时间**、迭代数、错误数、**复制积压**（已写完但尚未复制到目标桶的对象数）
- **达到 budget 即停**（主终止条件）；每完成一个对象写一次 checkpoint
- 结束打印 JSON 摘要（可直接喂下游分析）

### 4.2 `yo-s3 setup-crr`（一键配置跨区复制）

**需求：** 用户的桶还没配跨区复制，不想手动点控制台。

**行为：** 只问一个目标 region，其余自动：为源桶开版本控制 → 在目标 region 建目标桶并开版本控制 → 创建复制所需 IAM Role 与策略 → 写入复制规则（仅覆盖本工具前缀）。全程幂等，已就位则跳过。完成后提示可直接跑 `yo-s3`。

### 4.3 `yo-s3 cleanup`（手动清理）

**需求：** 程序被 `kill -9` 或不打算续跑，手动清掉残留。

**行为：** 扫描指定 bucket/prefix 下**本工具产生的**未完成 multipart 分段（`abort_multipart_upload`）与对象（可选），源桶与目标桶都清。删除前打印将删清单并确认。

### 4.4 全局开关

- `--dry-run`：走完整流程（校验、成本预估、限速、切片）但**不实际发 PUT**，用于验证参数与预估
- `--resume`：默认 checkpoint 路径存在时**自动交互询问**"上次跑到 3/20，继续还是重来？"，无需记参数；配置快照不一致则列 diff 报错，不静默继续
- `--yes`：跳过所有确认（无人值守）
- `--endpoint-url` / `--path-style` / `--insecure-skip-tls-verify`：S3 兼容存储（MinIO/Ceph）支持（见 §7）

---

## 5. 核心机制（复用高速写入设计）

「烧钱」的手段仍是高速写大对象，以下底层机制与定位无关、必须做对：

### 5.1 数据源：常驻内存 buffer 池
- 启动时一次性生成 `--pool-size`（默认 2 GiB）随机数据，存为单个 `Bytes`；用 `SmallRng` 按 CPU 核数**并行分块填充**（非密码学随机，不拖慢启动）
- 每个 part 从池中**随机偏移**处环形取段，`Bytes::slice()` 切片（O(1) refcount，**零拷贝**）
- 每个对象第一个 part 开头写入 64 字节唯一标识（magic + run_id + 迭代号 + 时间戳 + UUID），**使目标端任何去重/压缩失效**（去重会让"写入量"虚高、成本对不上，必须破除），同时便于追溯
- 池大小校验：必须 ≥ 2 × part_size

### 5.2 限速：字节级异步令牌桶
- 全局单桶，所有并发上传共享；令牌不足时 `tokio::time::sleep` 等待，**绝不自旋、绝不阻塞 sleep**
- 后台任务每 `--rate-resample-interval`（默认 30s）从 `[min, max]` 重采样速率、更新填充率；`--rate-mode` 支持 `continuous`（持续抖动，默认）与 `per-object`（每对象开始时定一次）
- 桶容量允许小幅突发（≤ 1s 配额），长期平均严格落在 `[min, max]`

### 5.3 上传：底层 multipart + 自建重试
- 直接用 `create_multipart_upload` / `upload_part` / `complete_multipart_upload`，**不用高层封装**；限速插在每次 `upload_part` 之前
- Body 用 `ByteStream` 承载 `Bytes`（非 `Vec<u8>`）；跨界两段 + 64B 头通过可重放的 `ChunkedBody` 拼接，**全程零实际数据拷贝**
- **SDK 重试全关**（`RetryConfig::disabled`），重试完全自建：503 SlowDown 单独计数 + 指数退避 + full jitter（cap 60s，默认 8 次）；鉴权/NoSuchBucket 等致命错误立即失败并给出凭据存储路径与重配命令
- 不碰 `Content-MD5`（MD5 单核几百 MB/s 会成瓶颈），默认让 SDK 用 CRC32

### 5.4 残片清理（省钱与正确性双重必需）
- **任何失败路径**（part 重试耗尽 / complete 失败 / task panic / SIGINT / --max-duration 到点）统一走 `UploadRegistry::abort_all()` 清理在途分段——残片不会自动消失、控制台看不见、但一直计费
- 进程被强杀的兜底两层：resume 启动时 `list_multipart_uploads` abort 本 run 孤儿 + 生成的 lifecycle JSON 带 `AbortIncompleteMultipartUpload`
- 生成建议 lifecycle policy JSON（含 `Expiration` / `NoncurrentVersionExpiration` / `AbortIncompleteMultipartUpload`，仅限本工具前缀）供手动应用；桶上已有规则则打印不覆盖

### 5.5 数据保留
- **默认不删已上传对象**；后台每 10 分钟扫描，物理删除本工具前缀下**超过 `--retain`（默认 24h）**的对象；开了版本控制时**按版本号彻底删除**（普通删除只生成 delete marker、旧版本继续计费）；源桶与目标桶都扫

### 5.6 终止条件
- **主**：`--budget` 达标（即时成本口径，见 §3）
- **可选次要边界**：`--total-size` / `--iterations` / `--max-duration`；`--stop-when all|any` 决定多条件关系（默认以 budget 为准，any 表示任一先到即停）

### 5.7 断点续跑
- checkpoint 写 JSON（原子写：临时文件 + rename）：run_id、已完成迭代数、已写入字节数、**已烧金额**、开始时间、有效运行时长、SlowDown 计数、config 快照
- 每完成一个对象写一次 + 退出时写一次；`--resume` 恢复，config 快照不一致列 diff 报错

### 5.8 指标与报告
- 周期性：瞬时/累计吞吐、实际 vs 目标速率、已烧 $ / 目标 $、迭代数、错误/SlowDown 计数、复制积压
- part 延迟直方图（`hdrhistogram`），最终报告 p50 / p95 / p99 / max
- 结束输出 JSON 摘要，字段结构化可直接分析

---

## 6. 安全护栏

- **预算硬上限**：budget 是硬性 ceiling，即时成本累加达标立即停止调度，不会"跑过头"
- **启动前成本预估 + 强制确认**：`--yes` 或交互确认才继续；`--dry-run` 全流程不发 PUT
- **版本控制警告**：检测到源/目标桶开启版本控制时显著提示"删除只生成 delete marker、旧版本继续计费，本工具按版本彻底删"
- **凭据零落盘**：不存任何 AWS 密钥，全走标准凭据链；启动打印当前认证身份，任何鉴权/权限失败都打印凭据来源与排查线索
- **只碰自己的前缀**：所有删除/清理仅限 `--key-prefix` 下本工具对象，不触碰桶内其他数据
- **NAT 流量提醒**：若 EC2 在私有子网经 NAT 访问 S3，会额外产生 NAT 流量费（工具无法自动检测，成本页文字提醒自查 VPC S3 Gateway Endpoint）

---

## 7. S3 兼容存储支持

- `--endpoint-url` 自定义端点；`--path-style` 切换寻址方式；设了 endpoint 时**自动**切 path-style + 兼容校验模式（`when_required`），减少输入
- `--insecure-skip-tls-verify`：首版仅接受 http:// 自定义端点（此时本就无 TLS）；https 自签端点会明确报错并提示改用 http 或将 CA 加入系统信任（aws-smithy-http-client 尚未暴露自定义证书验证器,不做假实现）
- **注意**：CRR 是 AWS 原生特性，MinIO/Ceph 等**不一定支持**。在兼容存储上工具退化为"纯写入 + 存储费"模式（烧钱慢，见 §3），成本预估页据实说明，不假装能跨区

---

## 8. 明确不做的事（已确认的坑）

1. 不往本地磁盘写大文件、不从磁盘读数据源 —— 全内存
2. 不在循环里生成随机数据 —— 只生成一次，之后全复用
3. 不克隆 buffer —— 任何 `.to_vec()` / `.clone()` 出实际拷贝都要有明确理由
4. 不强制 `Content-MD5` —— 用默认 CRC32
5. 不为分散前缀过度设计 —— 大对象场景 PUT 速率个位数/秒，离 3500/prefix 限制差三个数量级
6. 不用阻塞 sleep 或自旋限速
7. 不在缺清理逻辑的情况下退出

---

## 9. 设计决策

| 决策 | 理由 |
|------|------|
| 终止条件用 `--budget`（金额）而非数据量 | 用户真实目标是"花掉指定金额"，数据量只是手段；金额口径让工具"花够即停"，不多花不少花 |
| CRR 作默认核心引擎 | 三项成本量级验证：唯一能靠写入动作即时、大额、可精确控停地烧钱的是跨区流量费（$0.02/GB） |
| budget 只统计即时成本，存储费单列 | 存储费按时间发酵、工具停后仍涨，无法精确控停；计入会让"花够即停"失真 |
| 必填仅 budget + bucket，其余缺省 + 交互补齐 | 降低心智负担，零思考默认；漏填的必填项交互询问并给建议值 |
| checkpoint 每对象写一次 + 原子 rename | 连跑数小时到数天，随时可续；原子写防写坏 |
| SDK 重试全关、自建重试 | 否则 SlowDown 计数被 SDK 内层吞掉、双层重试互相放大退避 |
| 内置区域价格表按 `--region` 取，不暴露单价参数 | 少输入；区域不在表内回落 us-east-1 并提示核对 |
| 独立 binary | 与代理类工具零重叠，仅共享家族 CLI 风格 |

---

## 10. 复用与新增

**复用现有：** clap derive / colored 输出符号 / inquire 交互 / anyhow / thiserror / rand（需加 `small_rng` feature）/ tokio(full)；reqwest 已有 rustls，与 aws-sdk 一致。

**新增依赖：** `aws-config`、`aws-sdk-s3`、`aws-sdk-sts`（打印身份）、`aws-smithy-types`(http-body-1-x)、`http-body`、`bytes`、`uuid`(v4)、`indicatif`、`byte-unit`、`humantime`、`hdrhistogram`、`tracing`、`tracing-subscriber`(env-filter)、`tokio-util`(CancellationToken)。

**新增文件：**
```
src/bin/yo_s3.rs                 # clap 入口，子命令 run(默认) / setup-crr / cleanup
src/s3/
  mod.rs                         # 模块声明 + S3 硬限制常量 + 格式化工具
  config.rs                      # BenchConfig：校验 + config 快照 + 单位解析（含单元测试）
  client.rs                      # S3/STS client 构建：endpoint/path-style/checksum/重试关闭
  pool.rs                        # BufferPool：并行填充、环形取段、64B 唯一头（含单元测试）
  body.rs                        # ChunkedBody：Vec<Bytes> → 可重放 SdkBody（含单元测试）
  limiter.rs                     # 令牌桶 + 速率采样器（含长期均值单元测试）
  uploader.rs                    # 单对象 multipart 全流程 + part 重试退避 + SlowDown 分类
  registry.rs                    # 在途 upload_id 登记、abort_all、孤儿清理
  checkpoint.rs                  # 原子写、加载、快照一致性校验（含单元测试）
  metrics.rs                     # 原子计数、hdr 直方图、最终 JSON 摘要结构
  budget.rs                      # 实时成本累加器（µ$ 原子记账、尾对象精确缩小,含单元测试）
  cost.rs                        # 区域价格表、成本预估页、lifecycle JSON（含单元测试）
  crr.rs                         # 跨区复制检测 / 一键配置 / 复制积压采样
  sweep.rs                       # 保留期清扫：按版本物理删除超期对象（源/目标桶通用）
  commands/
    args.rs                      # clap 参数定义（run / setup-crr / cleanup）
    preflight.rs                 # 交互补齐、身份/桶/CRR 检查、成本页、确认门、checkpoint 决策
    run.rs                       # 调度循环、信号/采样器/报告器/清扫器、优雅退出、摘要
    setup_crr.rs / cleanup.rs / mod.rs
```
`src/lib.rs` 增 `pub mod s3;`；`Cargo.toml` 增 `[[bin]] name = "yo-s3"` 与新依赖；`CLAUDE.md` binary 列表补一行。

---

## 11. 当前已知限制

- **CRR 依赖 AWS**：兼容存储（MinIO/Ceph）不保证支持跨区复制，退化为纯写入烧钱（慢）
- **存储费不精确控停**：budget 只精确控制即时成本；存储费按保留期估算展示，不作停止判据
- **NAT 流量费不自动检测**：私有子网经 NAT 的额外流量费需用户自查
- **仅 Linux**：与家族其他工具一致（EC2 / Linux 主场）
