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

**结论**：光靠「写数据 + 留着」烧钱极慢（存储费按月发酵）。唯一能靠「写」这个动作**立即、大额、可精确控制到某一刻停下**的，是**跨区复制（Cross-Region Replication, CRR）产生的跨区流量费**。故 CRR 是 yo-s3 的**默认核心引擎**。

**预算口径（budget 统计哪些）**：默认只统计**即时成本** = 写入请求费 + 跨区流量费 + 目标端复制请求费。这三项随写入实时产生、可精确"花够即停"。**存储费不计入 budget 的停止判据**（它在工具停止后仍按时间累积，运行期无法精确控停），但会按保留期在成本预估页**单独估算展示**，让用户心里有数。

### 3.1 烧钱模式（`--mode`）：可插拔的成本引擎

「用哪个计费项当引擎」是唯一会随需求增长的维度，因此抽成 **mode**。一个 mode 只拥有三件事：

1. **开跑前要备什么资源**（`preflight`）——检测/创建复制目标桶等；
2. **字节怎么变成钱**（`cost_model`）——每字节的流量单价 + 每对象的额外请求数；
3. **一个工作单元怎么执行**（`run_unit`）——默认是 multipart 写一个对象。

其余全部**所有 mode 共用**：预算账本、令牌桶限速、内存数据池、checkpoint、指标与摘要、保留期清扫、信号处理与优雅退出。

| mode | 计费项 | 量级 | 预算能否精确控停 |
|------|--------|------|-----------------|
| `crr`（默认） | 跨区复制流量 | $0.02/GB，写完即产生 | ✅ |
| `write-only` | 仅 PUT 请求费 | ~$0.02/TiB | ❌ 需 `--total-size` / `--iterations` / `--max-duration` 收尾 |

**关键不变量**：

- **每字节费用是可叠加的列表，不是单项**（`CostModel.transfer: Vec<TransferFee>`）。同一个字节可以同时被多项计费：复制到 K 个区域、RTC、传输加速、上传路径过 NAT。预算按各项之和记账，成本预估页逐项列出。
- 预算账本（`budget.rs`）不认识任何具体 mode，它只消费 `CostModel`。判断「预算能否驱动停止」的唯一依据是**每字节成本合计是否 > 0**，而不是"是不是 CRR"——新 mode 自动被这条护栏覆盖。
- `crr` 模式没能配上复制时（用户选择跳过 / 自定义端点 / dry-run 读不到配置），它**如实退化**为纯请求费口径，不假装引擎在跑。
- mode 写入 checkpoint 快照：**换 mode 续跑会被拒绝并列 diff**，因为两种引擎的"已烧金额"口径不同，混算会让硬上限失真。旧版（无 mode 字段）checkpoint 按 `crr` 解释，可平滑续跑。

**扩展方式**：新增一个文件实现 `BurnMode` trait + 在 `ModeId::build` 加一个分支。

### 3.2 候选放大项（尚未实现）

这些不是新 mode，而是**在既有 mode 上叠加每字节费用**，因此都落在 `Vec<TransferFee>` 里：

| 放大项 | 单价（已核） | 形态 | 状态 |
|--------|-------------|------|------|
| 复制到 K 个目标区域 | $0.02/GB × K | `crr` 模式的参数 | ✅ **已实现**，见 §3.3 |
| Transfer Acceleration | **$0.04/GB**（美/欧/日边缘，其他 $0.08） | 上传路径附加费，与 mode 正交 | ✅ **已实现**，见 §3.4 |
| NAT 网关（非 VPC Endpoint） | **$0.045/GB**（非 $0.09） | 环境事实，**自动探测** | ✅ **已实现**，见 §3.5 |
| RTC（复制时间控制） | 待核（各区域同价） | 每条复制规则上的开关 | 未做：规则 `Destination` 加 `ReplicationTime` + `Metrics` 块 |

叠满 K=3 + RTC + TA 时每字节约 $0.19/GB，是单目标 $0.02/GB 的约 9.5 倍——对"快速烧掉到期 credits"的目标是数量级差异。

### 3.3 多目标复制（K 倍放大）

**AWS 已核事实**（决定了实现形状）：

- **同 scope、不同 destination 的多条规则会全部生效**——这正是 ×K 的机制：K 条规则共用同一个前缀 filter，各指一个目标桶。（若两条规则指向**同一个**目标桶，则只有 priority 最高的生效，故目标区域去重。）
- 跨区 DTO 对**每个**目标区域各收一次。
- 每个对象**每个目标**只计一次复制 PUT 请求费 → `requests_per_object = 2 + K`（create + complete + K）。
- 目标桶数量上限 = 该 partition 的区域数（商用分区当前 36 个区域，排除源区域后 K_max ≈ 35，可经 Service Quotas 提高）；每桶最多 1000 条复制规则，不构成瓶颈。opt-in 区域必须先在账户启用。

**默认 K = 5**（`DEFAULT_FANOUT`）。候选区域按序取前 5 个非源区域，全部是**默认启用的商用区域**（opt-in 区域会让没启用过的账户直接失败），且**刻意排除 us-east-2**——它与 us-east-1 是半价对，选它反而拖慢烧钱。默认集合有单测钉死（`default_fanout_sets_are_pinned`），因为它决定工具会真的在哪些区域建桶。

**计价按区域对求和，不是单价 × K**（关键修正）：

AWS 的计费用量类型是 `{源区域}-{目标区域}-AWS-Out-Bytes`，**按区域对计量**。基准费率由源区域决定（DTIR 降价公告确认「from any of these four regions to any other AWS region uses the listed rate」），但存在**折扣对例外**——已确认 `us-east-1 ↔ us-east-2 = $0.01/GB`，是标准价的一半，且官方措辞是「data sent between」，故双向对称。

因此 `CrrMode::cost_model` 对每个目标查 `crr_per_gb(pricing, dest_region)` 后**求和**，而不是 `源区域单价 × K`。目标区域探测不到时回落到源区域标准价——那是该源可能的最高价，折扣只会往下走，所以预算**永不少算**。

**实现**：

- `--dest-region` 可重复或逗号分隔：`yo-s3 --bucket b --budget 500 --dest-region us-west-2,eu-west-1,ap-south-1`
- 一个 IAM 角色覆盖全部目标（`ReplicateObject` 的 `Resource` 列出所有目标桶）。**漏一个目标，指向它的规则会静默不复制，而不复制的目标不产生费用**——所以角色策略必须一次性覆盖全部。
- 规则 ID `yo-s3-burn-{i}`、priority `1..K`，全部幂等。
- 清扫与 `yo-s3 cleanup` 覆盖**全部** K 个目标桶：漏掉一个就是一整份数据在那个区域永久计存储费。
- 存储费估算按 `1 + K` 份计。
- 拒绝目标区域 = 源区域（同区复制不产生跨区流量），拒绝重复区域。

**护栏**：预算硬上限在 K=1..4 下都有单测覆盖（`hard_ceiling_holds_at_every_fanout`）——K 同时改变每字节单价与每对象请求数，尾对象缩小的算式依赖后者，算错就会超预算花真钱。

### 3.4 传输加速（`--transfer-acceleration`，默认 `auto`）

**与 mode 正交**：它计的是上传腿本身，任何 mode 写的字节都要交这份钱。因此它不是 mode，而是在 `preflight` 里把一个 `TransferFee` **追加**到 mode 的 `CostModel` 上（`cost.transfer.extend(path_surcharges(..))`）。mode 只负责自己引擎的计费，不需要知道上传路径附加了什么。

**默认 `auto` 而非 `on`**：AWS 只在加速真正更快时才收这笔钱，而 yo-s3 最常见的部署是 EC2 与桶同区（为了吞吐），那种情况下加速费根本不产生。硬开会让预估里多出一笔永不兑现的 $0.04/GB，导致**提前约 29% 停机**。此外无脑打开还会让三类现在能跑的配置直接报错：桶名含点号、`--path-style` / `--endpoint-url`（MinIO）、桶在不支持加速的区域。

`AccelMode::Auto` 的语义是**能生效且会真正计费才启用**，任一不满足则静默退回 `false` 并打印一行原因，**不让运行失败**；`On` 保留严格语义（用户显式要求时，不可用就报错而不是悄悄降级）；`Off` 永不启用。裸写 `--transfer-acceleration`（不带值）等同 `on`，保持旧用法不破。

解析结果写回 `BenchConfig.transfer_acceleration`（含义是「本次运行实际武装了加速」，而非用户输入），因此 checkpoint 快照记录的是**实际计费口径**，续跑时口径变化会被拒绝。

叠加效果：`crr`(K=1) $0.02 + TA $0.04 = **$0.06/GB**，同样预算写入量降到 1/3。`write-only` + TA 也成立——TA 本身就能让预算精确控停（`ta_alone_can_drive_the_stop`）。

**AWS 已核约束**（都在启动前拦截，而不是逐请求失败）：

- 桶名**不能含点号**，且只支持 virtual-hosted 寻址 → 与 `--path-style`、`--endpoint-url` 互斥。
- 桶必须**先启用**加速（`s3:PutAccelerateConfiguration`）；未启用时交互询问是否当场开启，`--yes` 下拒绝并打印可直接复制的 `aws s3api` 命令（无人值守不擅自改桶配置）。开启后最多 20 分钟才完全生效。
- 只有 15 个区域支持，桶在别处直接报错。

**最大的坑（本功能一半代码是为它写的）**：AWS 官方原话 —— 判定加速"not likely to be faster"时**不收加速费**，甚至绕过加速系统。**客户端与桶同区时几乎必然落入此情形**：预估页照常显示这笔钱，实际账单为零，工具会报告"烧了"其实没花的钱。故同区时打印显式警告，说明想让这项真正生效，客户端要离桶足够远。

**客户端拆分**：加速端点只服务对象操作，不服务桶配置操作。因此 `RunContext` 持两个 client——`s3`（普通端点，负责发现/复制配置/清扫/积压采样）与 `upload_s3`（加速端点，只负责对象上传）。

**单价假设**：内置按美/欧/日边缘 $0.04/GB 计；实际由服务客户端的边缘节点决定，其他边缘是 $0.08/GB，预估页据实标注假设值。

### 3.5 NAT 网关处理费（自动探测，**无开关**）

**$0.045/GB** —— 这是四项里单价最高的一项，比 CRR 单目标（$0.02）和传输加速（$0.04）都贵。它计的是穿过 NAT 网关的每 GB（AWS 原话："regardless of the traffic's source or destination"），在账单上叫 **Data Processed by NAT Gateways**，不挂在任何 S3 条目下，极易被忽略。同区 EC2→S3 的数据传输本身免费，但只要路径经过 NAT，这笔处理费照收；换成免费的 **S3 Gateway Endpoint** 则归零。

（顺带纠一个常见混淆：**$0.09/GB 是 Data Transfer Out to Internet 的费率**，与 NAT 处理费是两个不同计费项。）

**为什么必须自动探测，不能给开关**：

- 让用户声明 = 把 VPC 拓扑知识变成使用门槛，违背「零思考默认」。
- 而且两个方向都会错：**漏算 → 实际花费 > 预算，突破硬上限**（危险方向）；**多算 → 提前停机，credits 没烧完**（达不成目的）。猜测在任一方向都是错的，所以去查。

**探测逻辑**（`netpath.rs`，全程失败降级、永不报错）：

1. IMDS 取 mac → subnet-id / vpc-id / region。**取不到 = 不在 EC2 上**（笔记本 / 本地），不可能有 NAT 费用，静默跳过。
2. `DescribeRouteTables` 按 `association.subnet-id` 找路由表；子网无显式关联时回落到 VPC 主路由表。
3. `DescribeVpcEndpoints` 查该路由表上有没有 `com.amazonaws.<region>.s3` 的 **Gateway** 型端点 —— 有则走免费路径。
4. 否则看**默认路由**（`0.0.0.0/0`）：指向 NAT → 计费；指向 IGW → 免费。

**三个容易做错的判断**：

- **只认默认路由**：S3 公网端点不在 VPC CIDR 内，只有 `0.0.0.0/0` 那条路由决定它怎么出去。指向 NAT 的窄 CIDR 路由（如 `10.1.0.0/16`）不承载 S3 流量，不能据此计费。
- **Gateway Endpoint 只覆盖同区**：桶在异区时，即使子网挂了 S3 endpoint，流量仍走 NAT。故先比对 `bucket_region` 与实例 region，不同则跳过 endpoint 判定。
- **「查不到」≠「没有」**：`DescribeVpcEndpoints` 失败（缺权限）时返回 `Unknown` 而不是「无端点」，否则会凭一次失败的 API 调用硬加一笔费用。`Unknown` 不加费，但打印一行说明账单可能比预估多。

**权限**：`ec2:DescribeRouteTables` + `ec2:DescribeVpcEndpoints`。**可选** —— 没有就降级为 `Unknown`，不影响主流程。

**输出克制**：只有需要用户知道的路径才打印。不在 EC2 / 走 IGW → 完全静默（不可能产生这笔费用，说了是噪音）；走 Gateway Endpoint → 一行 ✓ 确认免费；走 NAT → ⚠ 说明已计入并提示可加 endpoint 省钱；`Unknown` → 一行提示。

**依赖选择：用 `aws-sdk-ec2`，代价是构建时间而非体积**

Rust SDK 的分包粒度和 Java v2 一致 —— 每个服务一个 crate，`aws-sdk-ec2` 就是「EC2 产品的独立包」。但 **EC2 服务本身有 1512 个操作**，且 AWS **没有做操作级 feature gating**：crate 的 `[features]` 只有 `rustls` / `rt-tokio` / `default-https-client` 这类 TLS 与 runtime 开关，`default-features = false` 对体积零帮助。这不是「没找到更细的包」，而是 AWS 讨论过没做 —— 见 [awslabs/aws-sdk-rust#113](https://github.com/awslabs/aws-sdk-rust/issues/113)：用户提议按操作 feature gate，AWS 回复「will investigate」后无下文；有人尝试 fork 生成代码手工裁剪，因「how entangled the generated code is」放弃。

**评估过替代方案并实测否决**：直接讲 EC2 Query API（`Version=2016-11-15`）+ `aws-sigv4` 官方签名 + `xmlparser`（四个 crate 都已是 S3 SDK 的传递依赖）。实测结果：

| | `aws-sdk-ec2` | 手写 Query API |
|---|---|---|
| **发布二进制** | **18.93 MB** | 19.35 MB |
| release 构建 | 283 s | 128 s |
| 中间产物（debug rlib） | 1.27 GB | 0 |

**二进制反而更小**，原因有两层：`lto = true` + `strip = true` 本就把 1512 个未调用的操作全剥掉了，二进制里从来没装过它们；而手写版用 `reqwest` 发请求，会在 SDK 自带的 smithy/hyper 栈之外**再链接一整套 HTTP 栈**（`src/s3/` 其余部分完全不碰 reqwest）。理论上改用 smithy 已链接的客户端可以两头占优，但那需要在生产代码里构造 `RuntimeComponents`（orchestrator 内部机件，`for_tests()` 是测试专用），脆弱且不值。

**结论**：本项目以发布二进制体积为优先，构建时间可接受，故保留 `aws-sdk-ec2`；顺带也避免了手写 XML 解析的正确性风险。

**失败降级天然安全**：权限不足、IMDS 不可达、路由表读不到，任何一步失败都归为 `Unknown` → 不加费 → 退回本功能引入前的行为，不会把账算错。IMDS 探测加 2s 超时上限，实测 IMDS 地址被黑洞时零额外延迟。

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
  3. `HeadBucket` 连通性；**检测桶是否开启版本控制**（CRR 要求开，未开则在自动配置复制时帮开）
  4. **检测跨区复制是否已配置**：已配 → 显示目标桶 + 目标 region；未配 → 当场配好，见 §4.2
  5. **成本预估页**：按 region 真实单价，列出「预计写入总量、跨区流量费、请求费、（附带）保留期内存储费估算」，合计对齐到 `--budget`
  6. `--yes` 或交互确认后开跑
- 运行中每 `--report-interval`（默认 10s）打印一行：瞬时/累计吞吐、实际 vs 目标速率、**已烧 $X / 目标 $Y、预计剩余时间**、迭代数、错误数、**复制积压**（已写完但尚未复制到目标桶的对象数）
- **达到 budget 即停**（主终止条件）；每完成一个对象写一次 checkpoint
- 结束打印 JSON 摘要（可直接喂下游分析）

### 4.2 跨区复制的自动配置（并入 run，不是单独命令）

**需求：** 用户的桶还没配跨区复制，不想手动点控制台，**也不想为此记住第二个命令**。

**行为：** 桶未配复制时，`run` 的预检当场把它配好：为源桶开版本控制 → 在每个目标 region 建目标桶并开版本控制 → 创建覆盖全部目标的复制 IAM Role 与策略 → 写入 K 条复制规则（仅覆盖本工具前缀）。全程幂等，已就位则跳过。

目标区域从哪来，取决于**用户表达意图的方式**：

| 场景 | 目标区域来源 | 确认方式 |
|------|-------------|---------|
| 交互式，未给 `--dest-region` | 三选一菜单选「现在自动配置」→ 询问区域（默认填好 K=5） | 选菜单 + 填区域本身就是确认，不再多问一次 |
| 给了 `--dest-region` | 命令行 | 列出将创建的全部资源后 y/N（`--yes` 时跳过） |
| `--yes` / `--dry-run`，未给 `--dest-region` | —— | **不配置**，如实告知引擎缺失并提示加 `--dest-region` |
| 桶上已有复制配置 | 现有配置 | 不动它；`--dest-region` 打一行「本次不生效」而非静默忽略 |

**关键决策：`--dest-region` 就是授权本身。** 无人值守时凭空在 K 个区域建桶 + 建 IAM 角色，必须有一个明确的用户意图信号，点名区域就是那个信号；没点名就宁可不配、如实报告，而不是替用户拿主意。

**「只配好、这次先不烧」不需要单独命令**：配置发生在成本预估页与确认门**之前**，跑一次不带 `--yes` 的命令、在确认门答 `n` 即可。这正是删掉 `setup-crr` 子命令没有损失任何能力的原因。

### 4.3 `yo-s3 cleanup`（手动清理）

**需求：** 程序被 `kill -9` 或不打算续跑，手动清掉残留。

**行为：** 扫描指定 bucket/prefix 下**本工具产生的**未完成 multipart 分段（`abort_multipart_upload`）与对象（可选），源桶与目标桶都清。删除前打印将删清单并确认。同桶同前缀有实例正在跑时**拒绝执行**（清理分不清孤儿与在途分段），`--force` 才放行，见 §5.8。

**`--all`：连自动配置建出来的东西一起删。** 配置复制会在账号里留下 K 个目标桶、一个 IAM 角色 + 内联策略、源桶上 K 条复制规则；不删它们只是不计费，不是不存在。**源桶本身永不删除**。删除按依赖顺序：

1. 删源桶的复制规则 —— **必须最先做**，否则期间新写的对象还在往即将删除的桶里复制
2. **整桶清空**目标桶（不限于工具前缀，`DeleteBucket` 要求桶完全为空）
3. 删目标桶
4. 删角色内联策略 → 删角色（角色按源桶命名，一个源桶一个，删它不影响其他桶）

源桶版本控制不动（用户自己的桶，且版本控制只能暂停不能移除）。每步失败只报告不中断——半拆状态比尽力拆完更糟。自定义端点下直接报错。

**撞名防护：** 目标桶名是从源桶名推导的（`<源桶>-crr-<region>`），完全可能撞上用户原本就有的同名桶——`setup` 遇到已存在的桶是直接复用的。因此 `setup` 给**自己创建**的桶打 `yo-s3-created` 标签，`--all` 的确认清单把没有标签的桶单独标为「非本工具创建」。读标签失败按「非本工具创建」处理，是安全的失败方向。

### 4.4 全局开关

- `--mode`：选择烧钱模式（成本引擎），默认 `crr`，见 §3.1
- `--dest-region`：跨区复制目标区域（逗号分隔）。桶未配复制时它既是配置输入、也是「授权自动建桶+建 IAM 角色」的意图信号，见 §4.2
- `--transfer-acceleration`：上传走加速端点，每字节 +$0.04/GB，叠加在所选 mode 之上，见 §3.4
- `--dry-run`：走完整流程（校验、成本预估、限速、切片）但**不实际发 PUT**，用于验证参数与预估
- `--resume`：默认 checkpoint 路径存在时**自动交互询问**"上次跑到 3/20，继续还是重来？"，无需记参数；配置快照不一致则列 diff 报错，不静默继续
- `--checkpoint` / `--summary-out`：省略时落在状态目录 `~/.yo/s3/<桶>-<哈希>/`，见 §5.7
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

### 5.7 状态存储与断点续跑

**状态目录：`~/.yo/s3/<桶>-<哈希8>/`**，内含 `ckpt.json`、`summary.json`、`run.lock`（700 权限，沿用 `~/.yo/github` 约定）。目录身份是 **(endpoint, bucket, key_prefix) 的哈希**——即「一本预算账」所属的东西，而不是用户当时所在的工作目录。

- 早期版本默认写 `./yo-s3.ckpt.json`，等于把账本身份绑在 cwd 上：从两个目录起同一个桶就是两本账、同一笔预算烧两遍。cwd 里存在旧 checkpoint 时**自动接管并提示新位置**，老用户无感
- `--checkpoint` / `--summary-out` 显式指定时原样生效；`--resume <路径>` 未配 `--checkpoint` 时进度**写回被续的那个文件**，不写去别处
- `--dry-run` 用独立子目录 `.../dry-run/`：演练照样走调度、照样给没上传的对象记账，共用目录会把没花的钱写进真账本；独立目录同时意味着演练既不占也不等真实运行的锁

**checkpoint 内容**：run_id、已完成迭代数、已写入字节数、**已烧金额**、开始时间、有效运行时长、SlowDown 计数、config 快照。每完成一个对象写一次 + 退出时写一次；原子写（临时文件 + rename），**临时文件名带 pid**——否则两个进程的 `create` 会互相截断同一个临时文件，rename 过去的是撕裂内容。`--resume` 恢复，config 快照不一致列 diff 报错

### 5.8 单实例护栏

预算账本 = 一个 checkpoint 文件 + 每个进程各自内存里的 `BudgetMeter`，所以**两个进程共用一个状态目录就各烧一遍完整预算**——整个工具赖以成立的「预算是硬上限」静默失效，花的是真钱。而且同目录 + `--yes` 时第二个进程会自动续跑、共用 run_id，于是共用 `run_prefix()`：它启动时的孤儿清理会**直接 abort 掉第一个进程正在传的分段**，那些 part 的请求费和流量费已经付过了。

- 机制：状态目录下 `run.lock` 上的 **`flock`(LOCK_EX|LOCK_NB)**。选它而非 PID 文件的唯一理由是**内核在持有者死亡时自动释放**（`kill -9` 也算），因此没有陈旧锁要判、没有存活探测会判错
- **在任何 AWS 调用之前加锁**：第二个实例必须在能改桶的加速配置、能建复制目标桶之前就被拒
- 锁文件正文记 `cmd / pid / host / started_at`，拒绝时打印「谁在跑、跑了多久」而非干巴巴一句 busy
- 想并行请换 `--key-prefix`（各自独立的预算与清扫范围），`run` **不提供 `--force` 后门**
- `yo-s3 cleanup` 拿同一把锁：有实例在跑时拒绝执行（它的 `abort_orphans` 按整个前缀扫，分不清孤儿和在途），`--force` 才放行
- 文件系统不支持加锁时（NFS 无 lockd 等）降级为警告继续，不因为提供不了护栏就把正当运行挡死

### 5.9 指标与报告
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
- **NAT 流量自动计入**：若 EC2 在私有子网经 NAT 访问 S3，额外的 $0.045/GB 处理费会被自动探测并计入预算（见 §3.5），不再依赖用户自查 VPC

---

## 7. S3 兼容存储支持

- `--endpoint-url` 自定义端点；`--path-style` 切换寻址方式；设了 endpoint 时**自动**切 path-style + 兼容校验模式（`when_required`），减少输入
- `--insecure-skip-tls-verify`：首版仅接受 http:// 自定义端点（此时本就无 TLS）；https 自签端点会明确报错并提示改用 http 或将 CA 加入系统信任（aws-smithy-http-client 尚未暴露自定义证书验证器,不做假实现）
- **注意**：CRR 是 AWS 原生特性，MinIO/Ceph 等**不一定支持**。设了 `--endpoint-url` 时 `crr` 模式会如实退化为纯请求费口径（等价于 `--mode write-only`，烧钱慢，见 §3），成本预估页据实说明，不假装能跨区

---

## 8. 明确不做的事（已确认的坑）

1. 不往本地磁盘写大文件、不从磁盘读数据源 —— 全内存
2. 不在循环里生成随机数据 —— 只生成一次，之后全复用
3. 不克隆 buffer —— 任何 `.to_vec()` / `.clone()` 出实际拷贝都要有明确理由
4. 不强制 `Content-MD5` —— 用默认 CRC32
5. 不为分散前缀过度设计 —— 大对象场景 PUT 速率个位数/秒，离 3500/prefix 限制差三个数量级
6. 不用阻塞 sleep 或自旋限速
7. 不在缺清理逻辑的情况下退出
8. 不做跨机分布式锁 —— 单实例护栏只防本机重复启动，多台机器打同一桶+前缀仍会各花各的预算，见 §11

---

## 9. 设计决策

| 决策 | 理由 |
|------|------|
| 终止条件用 `--budget`（金额）而非数据量 | 用户真实目标是"花掉指定金额"，数据量只是手段；金额口径让工具"花够即停"，不多花不少花 |
| CRR 作默认核心引擎 | 三项成本量级验证：唯一能靠写入动作即时、大额、可精确控停地烧钱的是跨区流量费（$0.02/GB） |
| 引擎抽成 `--mode` + `BurnMode` trait | 「用哪个计费项烧钱」是唯一会持续增长的维度；原本"有没有 CRR"是一个散落在 budget / cost / preflight / reporter 四处的 bool，升格为 mode 后新增引擎只需一个文件 + 一个分支 |
| 护栏判据用"每字节成本是否 > 0"而非"是不是 CRR" | 「预算烧不满、永不停止」的根因是没有随字节线性产生的即时成本，与具体引擎无关；按根因写护栏，新 mode 自动被覆盖 |
| mode 进 checkpoint 快照 | 两种引擎的已烧金额口径不同，换 mode 续跑会让硬上限失真，必须显式拒绝而非静默继续 |
| budget 只统计即时成本，存储费单列 | 存储费按时间发酵、工具停后仍涨，无法精确控停；计入会让"花够即停"失真 |
| 必填仅 budget + bucket，其余缺省 + 交互补齐 | 降低心智负担，零思考默认；漏填的必填项交互询问并给建议值 |
| 删掉 `setup-crr` 子命令，折进 run | 它和 run 预检里的自动配置调的是同一个 `crr::setup`，交互式下纯冗余；唯一不可替代的 `--yes` 场景改由 `--dest-region` 覆盖。「只配不烧」用「配置早于确认门 → 答 n」实现，删命令零能力损失 |
| `--dest-region` 兼作无人值守的授权信号 | 凭空在 K 个区域建桶 + 建 IAM 角色不该在没人看着时自行发生；点名区域是最小、最自然的意图表达，比再加一个 `--auto-setup-crr` 开关少一个概念 |
| 拆除做成 `cleanup --all` 而非默认行为 | 删对象和删基础设施是两种不同性质的操作：前者可重来（下次接着烧），后者不可逆且要重配。默认只删数据，拆基础设施必须显式要 |
| `setup` 给自建的桶打 `yo-s3-created` 标签 | 目标桶名从源桶名推导，撞上用户已有同名桶完全可能，而 `setup` 对已存在的桶是直接复用的。没有这个标签，拆除逻辑就分不清「我建的」和「我借用的」，删桶就成了会毁用户数据的操作 |
| checkpoint 每对象写一次 + 原子 rename | 连跑数小时到数天，随时可续；原子写防写坏 |
| 状态目录身份用 (endpoint, bucket, prefix) 而非 cwd | 账本身份必须等于「这笔预算打在哪」；绑 cwd 时从两个目录起同一个桶就是两本账、同一笔预算烧两遍 |
| 单实例护栏用 flock 而非 PID 文件 | 内核在持有者死亡（含 kill -9）时自动释放，没有陈旧锁要判、没有存活探测会判错；代价只是 nix 加一个已有依赖的 feature |
| 加锁点在所有 AWS 调用之前 | 第二个实例必须在能改桶加速配置、能建复制目标桶之前就被拒，而不是走完预检才发现 |
| `run` 不给 `--force` 绕过锁 | 想并行有 `--key-prefix` 这个正当出口；防呆留后门等于没防。`cleanup` 才需要 `--force`（打断一个卡死的 run 是真实需求） |
| dry-run 独立状态子目录 | 演练会给没上传的对象记账，共用目录会把没花的钱写进真账本；独立目录顺带让演练不占真实运行的锁 |
| SDK 重试全关、自建重试 | 否则 SlowDown 计数被 SDK 内层吞掉、双层重试互相放大退避 |
| 内置区域价格表按 `--region` 取，不暴露单价参数 | 少输入；区域不在表内回落 us-east-1 并提示核对 |
| 独立 binary | 与代理类工具零重叠，仅共享家族 CLI 风格 |

---

## 10. 复用与新增

**复用现有：** clap derive / colored 输出符号 / inquire 交互 / anyhow / thiserror / rand（需加 `small_rng` feature）/ tokio(full)；reqwest 已有 rustls，与 aws-sdk 一致。

**新增依赖：** `aws-config`、`aws-sdk-s3`、`aws-sdk-sts`（打印身份）、`aws-smithy-types`(http-body-1-x)、`http-body`、`bytes`、`uuid`(v4)、`indicatif`、`byte-unit`、`humantime`、`hdrhistogram`、`tracing`、`tracing-subscriber`(env-filter)、`tokio-util`(CancellationToken)。

**新增文件：**
```
src/bin/yo_s3.rs                 # clap 入口，子命令 run(默认) / cleanup
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
  lock.rs                        # 单实例护栏：flock、持有者信息、不可加锁时降级（含单元测试）
  metrics.rs                     # 原子计数、hdr 直方图、最终 JSON 摘要结构
  budget.rs                      # 实时成本累加器（µ$ 原子记账、尾对象精确缩小,含单元测试）
  cost.rs                        # 区域价格表、CostModel、成本预估页、lifecycle JSON（含单元测试）
  crr.rs                         # 跨区复制的 AWS API 层：检测 / 一键配置 / 复制积压采样
  accel.rs                       # 传输加速：状态检测 / 开启 / 硬约束校验（含单元测试）
  netpath.rs                     # 出网路径自动探测：IMDS + 路由表 + S3 endpoint（含单元测试）
  modes/
    mod.rs                       # BurnMode trait + ModeId + DestTarget（含单元测试）
    crr.rs                       # mode `crr`：跨区复制流量引擎（含单元测试）
    write_only.rs                # mode `write-only`：纯请求费
  sweep.rs                       # 保留期清扫：按版本物理删除超期对象（源/目标桶通用）
  commands/
    args.rs                      # clap 参数定义（run / cleanup）
    preflight.rs                 # 交互补齐、身份/桶/CRR 检查、成本页、确认门、checkpoint 决策
    run.rs                       # 调度循环、信号/采样器/报告器/清扫器、优雅退出、摘要
    cleanup.rs / mod.rs
```
`src/lib.rs` 增 `pub mod s3;`；`Cargo.toml` 增 `[[bin]] name = "yo-s3"` 与新依赖；`CLAUDE.md` binary 列表补一行。

---

## 11. 当前已知限制

- **CRR 依赖 AWS**：兼容存储（MinIO/Ceph）不保证支持跨区复制，退化为纯写入烧钱（慢）
- **存储费不精确控停**：budget 只精确控制即时成本；存储费按保留期估算展示，不作停止判据
- **NAT 探测需 EC2 只读权限**：缺 `ec2:DescribeRouteTables` / `DescribeVpcEndpoints` 时降级为「未知」，此时预算不含 NAT 处理费，实际账单可能偏高
- **单实例护栏仅限本机**：`flock` 是本机文件锁，两台机器对同一桶+同一前缀各跑一个实例时，两边各记各的账、预算各花一遍。真要跨机并行请给每台一个独立 `--key-prefix`；运行时会打印这条限制。做成跨机需要 S3 条件写（`If-None-Match`）+ 心跳续租，目前不做
- **仅 Linux**：与家族其他工具一致（EC2 / Linux 主场）
