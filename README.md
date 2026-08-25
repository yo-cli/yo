# Linux - yo-git

```bash
# Build
cargo build --release --bin yo-git

# Install
sudo cp target/release/yo-git /usr/local/bin/yo
```

# Linux - yo-file

```bash
# Build
cargo build --release --bin yo-file

# Install
sudo cp target/release/yo-file /usr/local/bin/yo-file
```

# Linux / EC2 - yo-s3

按预算可控地消耗 AWS 成本:以受控随机速率向 S3 写入大对象,用跨区复制(CRR)流量费当烧钱主引擎,烧到指定金额自动停止。

```bash
# Build
cargo build --release --bin yo-s3

# Install
sudo cp target/release/yo-s3 /usr/local/bin/yo-s3
```

## ⚠ 成本提醒(先读这个)

- **这个工具的唯一目的就是真实花钱**,花出去的钱不可撤回。预算是硬上限,工具烧够即停,但存储费在停止后仍随保留时长少量累积(启动页有单独估算)
- 内置单价为近似值,**最终以 AWS 账单 / Cost Explorer 为准**
- EC2 在私有子网经 NAT 网关访问 S3 会**额外产生 NAT 流量费**(约 $0.045/GB,烧 $500 另付 ~$1100!),给 VPC 加免费的 **S3 Gateway Endpoint** 可完全避免
- 任何非正常退出后,跑一次 `yo-s3 cleanup --bucket <桶>` 清掉残留;启动时生成的 lifecycle JSON 建议手动应用作为最后兜底

## 三个典型场景

```bash
# 1. 第一次用:全交互,只回答几个问题(金额/桶名),其余全默认
#    桶不存在会问你要不要建;没凭据会让你选认证方式
yo-s3

# 2. 无人值守烧 $500(桶已配好跨区复制;nohup 后台跑,进度看日志)
nohup yo-s3 --budget 500 --bucket my-burn-bucket --yes > burn.log 2>&1 &

# 3. 桶还没配跨区复制:交互式跑会当场问你要不要配,配完接着烧
yo-s3 --budget 500 --bucket my-burn-bucket

# 4. 无人值守 + 桶还没配复制:点名目标区域,它自动配好再开烧
yo-s3 --budget 500 --bucket my-burn-bucket --dest-region us-west-2,eu-west-1 --yes
```

中断后续跑:直接重跑同一条命令,发现 checkpoint 会询问(或 `--yes` 自动)继续。

### 对象大小决定续跑粒度和进度可见度

`--object-size` 默认 **100 GiB**,它不只是「一次写多大」:

- **checkpoint 每完成一个对象写一次**。进程中途挂掉,当前对象的分段会被 abort,那部分进度全废,续跑从上一个完成的对象开始
- **跨区流量费也只在对象完成时记账** —— 因为 CRR 根本不复制未完成的对象,传一半被 abort 的字节 AWS 不收流量费。所以对象越大,「已烧」越久不动、动一次跳越大

按 K=5($0.10/GB)算:

| 对象大小 | 每个值多少钱 | 24h 规划下多久一个存档点 |
|---|---|---|
| 1 TiB | $102.40 | 4.4 小时 |
| **100 GiB(默认)** | **$10.24** | **26 分钟** |
| 10 GiB | $1.02 | 2.6 分钟 |

调小的代价只有每对象多几次 create/complete 请求 —— 按 $0.005/1000 算,整场跑下来是**千分之几美元**,完全可忽略。所以长跑就该用小对象。

运行时报告会把在途金额一起显示,不用等对象完成才知道在花钱:

```
📊 瞬时 307.00 MiB/s | 目标 384.24 MiB/s | 已烧 $20.48(在途 $10.24)/$500.00(4.1%) | 对象 2 完成
```

### 想让它跑满 N 小时:`--duration`

默认是**能多快烧多快**,$500 大约 4 小时就烧光了。想摊到一整天:

```bash
yo-s3 --budget 500 --bucket my-burn-bucket --duration 24h
```

```
ℹ 按 --duration 1day 规划:平均 65.83 MiB/s(区间 39.50 MiB/s – 92.16 MiB/s)
  预计写入总量:         5.42 TiB
  预计耗时:             1day
```

原理是一个除法:预算能买多少字节由单价决定、与速率无关,所以「要写的量 ÷ 目标时长 = 需要的速率」。不是边跑边调,开跑前就定死了。

- **它不是上限**。到点前预算烧完照样停 —— 想要「最长跑多久」的兜底是 `--max-duration`,那个到点会强制停、预算可能没烧完
- 速率仍然随机抖动(区间是 ±40%,均值正好落在目标上),只是抖动中心被挪到了推导出来的速率
- **续跑自动对齐**:速率恒定、剩余字节按比例减少,烧掉一半后重启,累计有效时长仍是你要的那个数
- 时长排得太短(推导速率超过 10 Gbps)会警告但不拦你 —— 达不到只是跑得比计划久,预算照样烧完
- `--mode write-only` 没有按字节计费,推不出写入量,需要配 `--total-size` 一起用

## 烧钱模式(`--mode`)

模式决定**用哪个 AWS 计费项当引擎**;预算记账、限速、断点续跑、清扫等其余机制所有模式共用。

| 模式 | 计费项 | 单价量级 | 能否靠预算精确控停 |
|------|--------|----------|-------------------|
| `crr`(默认) | 跨区复制流量 | 约 $0.02/GB × K 个目标区域(默认 K=5),写完即产生 | ✅ 主用模式 |
| `write-only` | 仅 PUT 请求费 | ~$0.02/TiB,存储费按月发酵 | ❌ 必须配 `--total-size` / `--iterations` / `--max-duration` |

`crr` 模式下若桶未配复制,交互会问是否当场配置;选择不配则自动退化为纯请求费口径,并如实展示"烧不动"。

### 多目标复制:烧钱速度 ×K(默认 K=5)

复制到 K 个目标区域,每个区域各收一次跨区流量费,烧钱速率就是 K 倍:

```bash
# 交互式:未配复制时会问你,默认填好 5 个目标区域 ≈ 5× 速度,可自行增删
yo-s3 --budget 500 --bucket my-burn-bucket

# 无人值守:点名区域即授权它自动建桶配规则
yo-s3 --budget 500 --bucket my-burn-bucket --dest-region us-west-2,eu-west-1 --yes
```

配置发生在成本预估页**之前**,所以「只配好复制、这次先不烧」也不需要单独的命令:跑一次不带 `--yes` 的命令,配完后在最后的确认门答 `n` 即可。

默认目标(源为 us-east-1 时):`us-west-2, eu-west-1, ap-northeast-1, ap-southeast-2, sa-east-1`。全是默认启用的商用区域,源区域会自动排除。opt-in 区域(af-south-1、me-south-1 等)需先在账户里启用才能用。

`--dest-region` 可逗号分隔,也可重复传。目标区域不能与源区域相同(同区复制不产生跨区流量),重复区域会被拒绝。

**费率按区域对算,不是简单乘 K**:基准由源区域决定,但有折扣对——`us-east-1 ↔ us-east-2` 只要 $0.01/GB(标准价一半)。工具会逐个目标查实际费率再求和,预估页也会逐条列出:

```
  复制目标桶:           5 个
    · my-burn-bucket-crr-us-west-2  (us-west-2, $0.0200/GB)
    · my-burn-bucket-crr-eu-west-1  (eu-west-1, $0.0200/GB)
    ...
```

所以默认 K=5 的候选里**故意不含 us-east-2**——选它反而因为半价而拖慢烧钱。

注意 K 也放大**存储费**:数据在 1 + K 个桶里各存一份。`--retain` 后台清扫和 `yo-s3 cleanup` 都会覆盖全部目标桶。

### 传输加速:默认自动叠加 +$0.04/GB

`--transfer-acceleration` 默认 `auto`——**能生效且 AWS 会真正计费时自动启用**,不用你操心:

```bash
# 默认就是 K=5 + 自动加速
yo-s3 --budget 500 --bucket my-burn-bucket
```

为什么不是无脑打开:**AWS 判定"加速不会更快"时不收加速费**。而 yo-s3 最常见的跑法就是 EC2 和桶同区(为了吞吐),那种情况下加速费根本不产生。硬开只会让预估里多出一笔永不兑现的钱,导致**提前约 29% 停机**。

`auto` 在下列任一情况下静默退回不启用,并打印一行原因,**不会让运行失败**:

- 客户端与桶同区(AWS 不会计费)
- 桶名含点号,或用了 `--path-style` / `--endpoint-url`(加速只支持 virtual-hosted 寻址)
- 桶所在区域不在支持加速的 15 个区域内
- 桶未启用加速,且处于 `--yes` 无人值守(不擅自改桶配置);交互模式下会问你是否现在开启

想强制或禁用:

```bash
yo-s3 --transfer-acceleration on    # 强制;不满足条件直接报错
yo-s3 --transfer-acceleration off   # 完全不用
```

> 想让加速真正烧钱,客户端要离桶足够远——例如 EC2 在 us-east-1、桶在 ap-southeast-2。

### NAT 网关处理费:自动探测,无需配置

如果 EC2 在私有子网、S3 流量经 NAT 网关出去,每 GB 还有 **$0.045** 的处理费——这是所有项里单价最高的,且在账单上叫 "Data Processed by NAT Gateways",不挂在 S3 名下,很容易漏看。

**你不需要做任何事**:工具启动时自动判断走的是 NAT 还是免费的 S3 Gateway Endpoint,该计的自动计进预算:

```
⚠ 检测到 S3 流量经 NAT 网关 —— 每字节额外 $0.045/GB,已计入预算。
  想省掉这笔钱:为该子网加一个免费的 S3 Gateway Endpoint
```

不在 EC2 上跑、或走公网 IGW 时,不可能有这笔费用,工具**完全不提**。

探测需要 `ec2:DescribeRouteTables` 和 `ec2:DescribeVpcEndpoints` 两个只读权限(见下方 IAM 段)。没有也能跑,只是会提示"无法确认是否走 NAT"。

> 顺带一提:**$0.09/GB 是 Data Transfer Out to Internet 的费率**,和 NAT 处理费是两回事,别混。

模式会写进 checkpoint 快照:**换模式续跑会被拒绝并列出 diff**(两种引擎的已烧金额口径不同,混算会失真)。

## 凭据:没配也能跑起来

工具走标准凭据链(EC2 IAM Role / 环境变量 / `~/.aws`),拿到就打印当前身份。**链上什么都没有时不会直接失败**,而是先看清你在什么环境,再给出这台机器上真正可行的选项:

```
✗ 没有可用的 AWS 凭据:这台机器在 EC2 上,但没挂 IAM Role(IMDS 返回 404)
? 怎么提供凭据?
> 粘贴 Access Key / Secret Key
  用已有 profile(default / prod)
  去控制台给这台实例挂 IAM Role(打印步骤后退出)
  退出
```

菜单是算出来的,不是固定的:不在 EC2 上就不会出现「挂 IAM Role」,`~/.aws` 里没 profile 就不会出现「用已有 profile」。选「挂 IAM Role」会打出带**你实际实例 ID** 的操作步骤。

- 粘贴的密钥**先过 STS 校验,通过了才问要不要记住**;不通过就重问,磁盘上不留任何东西
- 选择记住 → 写入 `~/.aws/credentials` 的 `[yo-s3]` profile(权限 600)。这是 AWS 工具链的标准位置,写完 `aws` CLI 也能用,不喜欢了直接 `vi` 删掉即可。**不会动你已有的任何 profile**
- 下次用 `--profile yo-s3` 直接指定,跳过菜单
- **`--yes` 下绝不弹窗**:无人值守时如实报错退出 —— nohup 跑了一半停下来等输入比失败更糟
- 只缺区域(凭据是好的)时只问区域,不会让你去粘根本不需要的密钥

给 EC2 挂 IAM Role 仍然是最省事的做法(临时凭据自动轮换,不落任何长期密钥)。Role 最小权限:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "s3:ListBucket", "s3:ListBucketMultipartUploads", "s3:ListBucketVersions",
        "s3:GetBucketLocation", "s3:GetBucketVersioning", "s3:GetReplicationConfiguration",
        "s3:PutObject", "s3:AbortMultipartUpload", "s3:ListMultipartUploadParts",
        "s3:DeleteObject", "s3:DeleteObjectVersion", "s3:GetObject"
      ],
      "Resource": ["arn:aws:s3:::<源桶>", "arn:aws:s3:::<源桶>/*",
                    "arn:aws:s3:::<每个目标桶>", "arn:aws:s3:::<每个目标桶>/*"]
    }
  ]
}
```

**自动配置跨区复制**(交互确认或 `--dest-region`)需要额外的一次性较高权限(`s3:CreateBucket`、`s3:PutBucketVersioning`、`s3:PutBucketTagging`、`s3:PutReplicationConfiguration`、`iam:CreateRole`、`iam:PutRolePolicy`、`iam:GetRole`、`iam:PassRole`),建议用管理员身份先跑一次把复制配好;之后的烧钱运行只需下面的常规权限。

**`cleanup --all`** 还需要拆除权限:`s3:GetBucketTagging`、`s3:DeleteBucket`、`s3:DeleteBucketReplication`、`iam:DeleteRole`、`iam:DeleteRolePolicy`。

**可选(建议加上)**:`ec2:DescribeRouteTables`、`ec2:DescribeVpcEndpoints` —— 用于自动判断 S3 流量是否经 NAT 网关($0.045/GB)。缺这两个权限工具照常运行,但预算里不会包含 NAT 处理费,实际账单可能偏高。

`--transfer-acceleration` 需要 `s3:GetAccelerateConfiguration`;若要让工具帮你开启加速,还需 `s3:PutAccelerateConfiguration`。

## 参数说明

必填只有两个,漏了会交互询问;其余全部有零思考默认值:

| 参数 | 默认 | 说明 |
|------|------|------|
| `--budget <N>` | 交互询问 | 要烧掉的美元金额,硬上限,烧够即停 |
| `--bucket <名>` | 交互询问 | 目标 S3 桶 |
| `--mode` | `crr` | 烧钱模式(成本引擎),见下表 |
| `--profile` | 无 | 用 `~/.aws` 里的哪个 profile。省略走标准凭据链,链上没凭据时交互模式会让你选或粘贴 |
| `--key-prefix` | `yo-s3-bench/` | 所有写入/清理只发生在该前缀下;也是单实例锁与状态目录的身份之一 |
| `--dest-region` | 无 | 跨区复制目标区域(逗号分隔)。桶未配复制时,给了它就自动建目标桶+IAM角色+复制规则再开烧;已配则以现有配置为准 |
| `--object-size` | `100GiB` | 单对象大小。同时决定续跑粒度与预算显示步长,见下 |
| `--part-size` | `256MiB` | multipart 分片大小(S3 限制自动校验) |
| `--pool-size` | `2GiB` | 常驻内存随机数据池(须 ≥ 2×part) |
| `--concurrent-objects` / `--concurrent-parts` | `1` / `4` | 两级并发 |
| `--rate-min` / `--rate-max` | `200MiB` / `500MiB` | 速率区间(字节/秒),上传速率在区间内随机波动 |
| `--rate-mode` | `continuous` | `continuous` 每 30s 换速 / `per-object` 每对象定一次 |
| `--rate-resample-interval` | `30s` | continuous 模式换速间隔 |
| `--retain` | `24h` | 对象保留时长,后台每 10 分钟物理删除超期版本(源桶+目标桶);`0s` = 永不删 |
| `--duration` | 无 | **规划**用多久烧完(如 `24h`),速率由此反推。与 `--rate-min`/`--rate-max` 互斥 |
| `--total-size` / `--iterations` / `--max-duration` | 无 | 可选**边界**(到点/到量强制停,预算可能没烧完);`--stop-when any` 时任一先到即停 |
| `--checkpoint` | 状态目录下 `ckpt.json` | 每完成一个对象原子写一次;存在即可续跑 |
| `--summary-out` | 状态目录下 `summary.json` | 结束时的机器可读摘要 |
| `--report-interval` | `10s` | 运行报告间隔 |
| `--transfer-acceleration` | `auto` | 传输加速 +$0.04/GB。`auto` 能生效且会真正计费时自动启用 / `on` 强制 / `off` 关闭 |
| `--endpoint-url` / `--path-style` | 无 | S3 兼容存储(MinIO/Ceph);设 endpoint 自动切 path-style + 兼容校验模式。注意兼容存储无 CRR,烧钱极慢 |
| `--dry-run` | 关 | 全流程演练,不发任何真实写入 |
| `--yes` | 关 | 跳过所有确认(无人值守) |

子命令只剩一个:`cleanup`(手动清残留分段上传 + 物理删除本工具前缀下对象,源/目标桶都清)。跨区复制的配置已并入主命令,见 `--dest-region`。

## 用完收摊:`cleanup --all`

自动配置跨区复制会在你账号里留下东西:**K 个目标桶**(`<源桶>-crr-<region>`)、**一个 IAM 角色**(`yo-s3-crr-<源桶>`)+ 内联策略、**源桶上的 K 条复制规则**。不加 `--all` 的 `cleanup` 只删对象,这些会一直留着。

```bash
# 只删对象(目标桶和角色保留,下次还能接着烧)
yo-s3 cleanup --bucket my-burn-bucket

# 连基础设施一起拆干净
yo-s3 cleanup --bucket my-burn-bucket --all
```

拆除顺序:删源桶复制规则 → 整桶清空目标桶 → 删目标桶 → 删角色策略 → 删角色。先停复制再删桶,否则期间新写的对象还在往即将消失的桶里流。

几点要知道:

- **目标桶会被整桶清空**,不只是本工具前缀 —— `DeleteBucket` 要求桶完全为空
- **源桶只有在本工具创建时才会被删**(靠 `yo-s3-created` 标签判断)。你自带的桶永远保留
- 工具给**自己创建**的桶(源桶和目标桶都算)打了 `yo-s3-created` 标签。目标桶名是从源桶名推导的,可能撞上你原本就有的同名桶;拆除前的清单会把没有标签的桶单独标出来,让你看清楚再决定
- 源桶的版本控制**不会**被关掉(那是你自己的桶,且版本控制只能暂停不能移除)
- 不可恢复,默认要交互确认;`--yes` 可跳过
- 自定义端点(MinIO/Ceph)下 `--all` 直接报错:那里本来就没有跨区复制

## 排查:跑挂了怎么看

运行中每 `--report-interval` 一行:

```
📊 瞬时 307 MiB/s | 目标 384 MiB/s | 已烧 $20.48(在途 $10.24)/$500.00(4.1%) | 对象 2 完成 | 重试 119 | SlowDown 0
```

| 字段 | 看什么 |
|---|---|
| 瞬时 vs 目标 | **实际持续低于目标一半就是麻烦** —— 网络跟不上,part 会排队积压 |
| 在途 | 当前对象已预留的金额,对象完成才转成「已烧」 |
| 重试 | 持续增长 = 连接在反复失败,通常是上一条的后果 |
| SlowDown | S3 在限流你(和网络瓶颈是两回事) |

**吞吐持续不足会主动告警**,不用等它崩:

```
⚠ 实际吞吐 100 MiB/s 持续低于目标 384 MiB/s 的一半 —— 网络已是瓶颈。
  继续下去 part 会排队积压,连接被饿死后 SDK 判定失速并中止上传。
  建议:降到实测水平(--rate-min 60MiB --rate-max 90MiB),或用 --duration <时长> 让它自己推导速率
```

**单个 part 重试到第 3 次起会打印**(前两次是常态噪音,不打)。默认日志级别就能看到,不用调 `RUST_LOG`。

**对象失败时给全上下文**,不再让你去翻日志:

```
✗ yo-s3-bench/75cb8faf/obj-000003 上传失败已中止(完成 12/4096 个 part,耗时 843s,
  当前目标速率 384.24 MiB/s): UploadPart #13 重试 8 次后仍失败: ...
```

想看更细的可以 `RUST_LOG=debug`,会打出每一次重试和退避时长。

### 常见死法

**`ThroughputBelowMinimum` / `minimum throughput was specified at 1 B/s`**
不是权限问题,是 AWS SDK 的失速保护:连接连续 5 秒零字节就判死。根因几乎总是**目标速率超过实例网络基线** —— EC2 超出带宽配额时会先排队再丢包,连接因此饿死。

确认方法(需要 ENA 驱动 2.2.10+):

```bash
ethtool -S eth0 | grep bw_out_allowance_exceeded   # 这个数在涨 = 实锤被限速
```

查你这台机器的基线带宽(**控制台不显示,只能用 CLI**):

```bash
aws ec2 describe-instance-types --filters "Name=instance-type,Values=m5.*" \
  --query "InstanceTypes[].[InstanceType, NetworkInfo.NetworkPerformance, NetworkInfo.NetworkCards[0].BaselineBandwidthInGbps]" \
  --output table
```

解法是降速到基线以下,不是提配额 —— **网络带宽不是 Service Quota,提不了工单**。

## 状态存储与单实例护栏

运行状态放在 **`~/.yo/s3/<桶>-<哈希>/`**:`ckpt.json`(断点续跑)、`summary.json`、`run.lock`。目录身份是 `(endpoint, bucket, key-prefix)` 的哈希 —— 一本预算账对应一个目录,跟你在哪个工作目录敲命令无关。

- **同桶同前缀只允许一个实例在跑**。第二个会在任何 AWS 调用之前被拒,并告诉你谁在跑(pid / 主机 / 已运行多久)。因为两个实例各记各的账,`--budget` 的硬上限会被花掉两遍
- 想并行请给每个实例换一个 `--key-prefix`,各自独立的预算与清扫范围。`run` 没有 `--force` 后门
- `cleanup` 拿同一把锁:有实例在跑时拒绝执行(它会 abort 对方的在途分段),`--force` 才放行
- 锁用 `flock`,进程被 `kill -9` 也由内核释放,不存在需要手动清的陈旧锁
- **仅防本机**:两台机器打同一个桶+前缀仍会各花各的预算,这种场景请给每台一个独立前缀
- `--dry-run` 用独立子目录,演练的账永远进不了真账本
- 早期版本写在 `./yo-s3.ckpt.json`;当前目录下有旧文件时会自动接管并提示新位置
