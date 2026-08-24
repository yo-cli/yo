// Clap argument structs. Only --budget and --bucket are "required" — and even
// they are prompted interactively (with suggested defaults) when omitted.
// Everything else has a zero-thought default.

use clap::Args;
use std::time::Duration;

use crate::s3::config::{
    parse_duration, parse_rate, parse_size, parse_usd, AccelMode, RateMode, StopWhen,
};
use crate::s3::modes::ModeId;

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// 烧钱模式(成本引擎):crr 跨区复制流量 / write-only 纯写入
    #[arg(long, value_enum, default_value_t = ModeId::Crr)]
    pub mode: ModeId,

    /// 要烧掉的预算金额(美元)。省略时交互询问
    #[arg(long, value_parser = parse_usd)]
    pub budget: Option<u64>,

    /// 目标 S3 桶。省略时交互询问
    #[arg(long)]
    pub bucket: Option<String>,

    /// 对象 key 前缀(所有写入/清理只发生在该前缀下)
    #[arg(long, default_value = "yo-s3-bench/")]
    pub key_prefix: String,

    /// 跨区复制目标区域,可重复或逗号分隔。桶还没配复制时,给了它就自动配好
    /// (建目标桶 + IAM 角色 + 复制规则)再开烧;每多一个区域烧钱速度加一倍。
    /// 桶上已有复制配置时以现有配置为准,此参数不生效
    #[arg(long = "dest-region", value_delimiter = ',')]
    pub dest_regions: Vec<String>,

    /// AWS 区域(默认走凭据链/EC2 元数据自动解析)
    #[arg(long)]
    pub region: Option<String>,

    /// 单对象大小
    #[arg(long, value_parser = parse_size, default_value = "1TiB")]
    pub object_size: u64,

    /// multipart 分片大小
    #[arg(long, value_parser = parse_size, default_value = "256MiB")]
    pub part_size: u64,

    /// 常驻内存随机数据池大小(须 ≥ 2 × part)
    #[arg(long, value_parser = parse_size, default_value = "2GiB")]
    pub pool_size: u64,

    /// 并发上传的对象数
    #[arg(long, default_value_t = 1)]
    pub concurrent_objects: usize,

    /// 单对象内并发上传的分片数
    #[arg(long, default_value_t = 4)]
    pub concurrent_parts: usize,

    /// 速率下限(如 200MiB 或 200MiB/s)
    #[arg(long, value_parser = parse_rate, default_value = "200MiB")]
    pub rate_min: u64,

    /// 速率上限
    #[arg(long, value_parser = parse_rate, default_value = "500MiB")]
    pub rate_max: u64,

    /// 速率模式:continuous 持续抖动 / per-object 每对象定一次
    #[arg(long, value_enum, default_value = "continuous")]
    pub rate_mode: RateMode,

    /// continuous 模式下速率重采样间隔
    #[arg(long, value_parser = parse_duration, default_value = "30s")]
    pub rate_resample_interval: Duration,

    /// 对象保留时长,超过后由后台清扫物理删除(0s = 永不删除)
    #[arg(long, value_parser = parse_duration, default_value = "24h")]
    pub retain: Duration,

    /// 可选边界:总写入量(如 20TiB)
    #[arg(long, value_parser = parse_size)]
    pub total_size: Option<u64>,

    /// 可选边界:迭代(对象)次数
    #[arg(long)]
    pub iterations: Option<u64>,

    /// 多个停止条件的关系(预算恒为硬上限)
    #[arg(long, value_enum, default_value = "all")]
    pub stop_when: StopWhen,

    /// 兜底最长运行时长(跨 resume 累计),到点强制优雅退出
    #[arg(long, value_parser = parse_duration)]
    pub max_duration: Option<Duration>,

    /// checkpoint 文件路径(默认 ~/.yo/s3/<桶>-<哈希>/ckpt.json,存在时启动会询问是否续跑)
    #[arg(long)]
    pub checkpoint: Option<String>,

    /// 显式从指定 checkpoint 续跑
    #[arg(long)]
    pub resume: Option<String>,

    /// 结束时 JSON 摘要输出路径(默认 ~/.yo/s3/<桶>-<哈希>/summary.json)
    #[arg(long)]
    pub summary_out: Option<String>,

    /// 运行报告打印间隔
    #[arg(long, value_parser = parse_duration, default_value = "10s")]
    pub report_interval: Duration,

    /// S3 兼容存储的自定义端点(MinIO/Ceph;设了自动切 path-style)
    #[arg(long)]
    pub endpoint_url: Option<String>,

    /// 强制 path-style 寻址
    #[arg(long)]
    pub path_style: bool,

    /// 跳过 TLS 证书验证(仅自建 http 测试环境)
    #[arg(long)]
    pub insecure_skip_tls_verify: bool,

    /// 传输加速(+$0.04/GB,叠加在所选 mode 之上):auto 能生效且会计费时自动启用 /
    /// on 强制 / off 关闭。裸写 --transfer-acceleration 等同 on
    #[arg(
        long,
        value_enum,
        num_args = 0..=1,
        default_value = "auto",
        default_missing_value = "on"
    )]
    pub transfer_acceleration: AccelMode,

    /// 全流程演练:不发任何真实写入请求
    #[arg(long)]
    pub dry_run: bool,

    /// 跳过所有交互确认(无人值守)
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug, Clone)]
pub struct CleanupArgs {
    /// 目标桶(省略时交互询问)
    #[arg(long)]
    pub bucket: Option<String>,

    /// 只清理该前缀下本工具产生的数据
    #[arg(long, default_value = "yo-s3-bench/")]
    pub key_prefix: String,

    /// AWS 区域(默认自动解析)
    #[arg(long)]
    pub region: Option<String>,

    /// S3 兼容存储的自定义端点
    #[arg(long)]
    pub endpoint_url: Option<String>,

    /// 强制 path-style 寻址
    #[arg(long)]
    pub path_style: bool,

    /// 跳过 TLS 证书验证(仅自建 http 测试环境)
    #[arg(long)]
    pub insecure_skip_tls_verify: bool,

    /// 把自动配置跨区复制时建的东西一起删掉:复制规则 + 目标桶(整桶清空后删除)
    /// + 复制 IAM 角色。不加时只删对象,这些会一直留在账号里。源桶本身永不删除
    #[arg(long)]
    pub all: bool,

    /// 即使检测到同桶同前缀有实例在跑也强制清理(会 abort 它的在途分段)
    #[arg(long)]
    pub force: bool,

    /// 跳过确认
    #[arg(long, short = 'y')]
    pub yes: bool,
}
