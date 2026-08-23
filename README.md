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
yo-s3

# 2. 无人值守烧 $500(桶已配好跨区复制;nohup 后台跑,进度看日志)
nohup yo-s3 --budget 500 --bucket my-burn-bucket --yes > burn.log 2>&1 &

# 3. 桶还没配跨区复制:先一键配置,再开烧
yo-s3 setup-crr --bucket my-burn-bucket        # 只问一个目标区域
yo-s3 --budget 500 --bucket my-burn-bucket
```

中断后续跑:直接重跑同一条命令,发现 checkpoint 会询问(或 `--yes` 自动)继续。

## EC2 凭据(不用配 KEY)

给实例挂一个 IAM Role 即可,工具经标准凭据链自动获取临时凭据并打印当前身份。Role 最小权限:

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
                    "arn:aws:s3:::<目标桶>", "arn:aws:s3:::<目标桶>/*"]
    }
  ]
}
```

`setup-crr` 需要额外的一次性较高权限(`s3:CreateBucket`、`s3:PutBucketVersioning`、`s3:PutReplicationConfiguration`、`iam:CreateRole`、`iam:PutRolePolicy`、`iam:GetRole`、`iam:PassRole`),建议在管理员身份下跑一次。

## 参数说明

必填只有两个,漏了会交互询问;其余全部有零思考默认值:

| 参数 | 默认 | 说明 |
|------|------|------|
| `--budget <N>` | 交互询问 | 要烧掉的美元金额,硬上限,烧够即停 |
| `--bucket <名>` | 交互询问 | 目标 S3 桶 |
| `--key-prefix` | `yo-s3-bench/` | 所有写入/清理只发生在该前缀下 |
| `--object-size` | `1TiB` | 单对象大小 |
| `--part-size` | `256MiB` | multipart 分片大小(S3 限制自动校验) |
| `--pool-size` | `2GiB` | 常驻内存随机数据池(须 ≥ 2×part) |
| `--concurrent-objects` / `--concurrent-parts` | `1` / `4` | 两级并发 |
| `--rate-min` / `--rate-max` | `200MiB` / `500MiB` | 速率区间(字节/秒),上传速率在区间内随机波动 |
| `--rate-mode` | `continuous` | `continuous` 每 30s 换速 / `per-object` 每对象定一次 |
| `--rate-resample-interval` | `30s` | continuous 模式换速间隔 |
| `--retain` | `24h` | 对象保留时长,后台每 10 分钟物理删除超期版本(源桶+目标桶);`0s` = 永不删 |
| `--total-size` / `--iterations` / `--max-duration` | 无 | 可选边界;`--stop-when any` 时任一先到即停 |
| `--checkpoint` | `./yo-s3.ckpt.json` | 每完成一个对象原子写一次;存在即可续跑 |
| `--summary-out` | `./yo-s3-summary.json` | 结束时的机器可读摘要 |
| `--report-interval` | `10s` | 运行报告间隔 |
| `--endpoint-url` / `--path-style` | 无 | S3 兼容存储(MinIO/Ceph);设 endpoint 自动切 path-style + 兼容校验模式。注意兼容存储无 CRR,烧钱极慢 |
| `--dry-run` | 关 | 全流程演练,不发任何真实写入 |
| `--yes` | 关 | 跳过所有确认(无人值守) |

子命令:`setup-crr`(一键配跨区复制)、`cleanup`(手动清残留分段上传 + 物理删除本工具前缀下对象,源/目标桶都清)。
