# Solana 原始 Shred 解码工具

[English](README.md) · [SolBrisk 官网](https://solbrisk.com/) · [接入文档](https://solbrisk.com/docs)

接收原始 UDP Merkle Shred，重组完整 Entry 批次，利用编码分片纠删恢复，并提取
交易、指令和账户信息。支持 legacy、v0、v1 交易、乱序、重复、半途接入、录制和回放。
由 [SolBrisk](https://solbrisk.com/) 提供，支持其他兼容原生数据的服务商。
需要 FRA/AMS Raw Shred 服务，可前往[官网](https://solbrisk.com/)创建订阅。

## 编译与运行

需要 Rust 1.89+ 和 C 工具链，无需运行验证器、RocksDB 或配置 RPC。

```bash
cargo build --release --locked
./target/release/solana-shred-decoder \
  --bind 0.0.0.0:21001 --duration 60 \
  --raw-transactions --output entries.jsonl --capture shreds.dat
```

将订阅接收端设置为**接收服务器公网 IP:21001**，本机防火墙允许该 UDP 端口。
`0.0.0.0` 只是本地监听地址，不应填写到订阅目的地址。请选择空闲端口；程序独占绑定，
不会抢占现有交易接收程序，不修改转发规则、系统服务或 sysctl。
`--duration 0` 持续接收，Ctrl+C 结束。程序能打印 SolBrisk 端点验证包的 48 位验证码，
保持接收运行并将验证码填回网页即可。`--source-ip` 可选限制发送方，应填写服务商
确认的来源地址，不要把 gRPC 域名对应 IP 直接当作 Shred 来源。

## 离线回放

```bash
./target/release/solana-shred-decoder \
  --input shreds.dat --raw-transactions --output replay.jsonl
```

录制格式为重复的 `u32 小端长度 + 原始 UDP 载荷`，不是 PCAP，不含时间戳。
回放不占 UDP 端口。截断、长度非法会报错；输出和录制文件已存在时拒绝覆盖。
生成一个含合成交易和 Tick 的本地样例：

```bash
cargo run --locked --example make_capture -- demo.dat
./target/release/solana-shred-decoder --input demo.dat --raw-transactions
```

合成样例使用占位签名，仅用于解码演示，不能当作有效链上交易发送。

## 输出内容与边界

- 每个 Entry 一行 JSON，包含 Slot、分片范围、批次内序号、PoH Hash、Tick 标记。
- 交易包含签名、版本、静态账户、指令、v0 ALT 描述。`--raw-transactions` 添加
  原始交易编码的 base64；程序不会发送交易。
- **解码不代表交易已成功、确认或最终确定**，输出明确保留 `execution_status=unknown`。
  不执行交易、不重放 Bank、不解析各个 DEX 的业务账户，也不做验签节点。
- 不调用 RPC 解析 ALT。若指令程序位于动态加载账户，`program_id` 可能为空，
  需结合独立、正确时点的 ALT 状态自行解析。
- 只支持当前链式 Merkle Shred，旧式非 Merkle 和商家自定义外层封装会拒绝。

## 纠删恢复与诊断

编码几何一致、有效分片足够时尝试恢复。恢复后重新计算整组 Merkle Root 并检查
头字段，不能把不同 root/尾部格式的分片混合恢复。若中继尾部表示不一致，仅禁用该
组 FEC 恢复，直接收到且业务载荷一致的数据仍可解析；载荷或 leader 签名冲突则隔离
整个 Slot。程序不获取 Leader Schedule，也不验证 Leader 的 Ed25519 签名。
格式校验不等于真实性认证，接入和自动交易前需自行建立可信数据边界。

支持序列化批次后的零填充，非零尾部会拒绝。去重仅覆盖缓存保留期，长期存储应自行去重。

统计有：非法包、重复、载荷冲突、`fec_identity_conflicts`、恢复量、恢复失败、批次、
Entry、交易和缓存淘汰量。半途启动可能跳过第一个不完整批次，下一处完整边界后恢复。
冗余不足无法补全，不会虚构交易。

默认保留 16 个 Slot、15 秒、64 MiB 计入的载荷缓存。可调 `--max-slots`、
`--slot-ttl`、`--max-buffer-mib`；这些不等于完整进程内存上限，Map/JSON 还有额外开销。
`--receive-buffer-mib` 可能被操作系统限制，启动会显示实际值。
文件写入量没有自动轮转，长期运行请设置时长或自行轮转。

无完整批次时退出码非零；已解出数据后，错误体现在统计中，退出码为零不代表无漏数。
完整格式与可信边界见 [FORMAT.md](FORMAT.md)。

## 验证

```bash
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

包括缺失恢复、仅编码分片恢复、重新签名格式、重复/冲突、晚接入及三种交易版本。
MIT 许可证，第三方说明见 [THIRD-PARTY.md](THIRD-PARTY.md)。
客服：[Discord 工单](https://discord.gg/P9jep5pvky) / support@solbrisk.com。
