# 外部适配器能力基线（R-001）

状态：基于 RMUX `0.8.0`、提交 `3b254fee1931` 完成。

本文档只记录可通过公开 Rust SDK 或 typed IPC 使用、并有可执行测试证明的行为。它是未来独立适配器的依赖边界，不定义 AI 智能体协议、全局消息总线或输入投递策略。

## 状态定义

- `supported`：公开接口足够，并有可执行证据。
- `partial`：存在可用公开能力，但仍有明确语义缺口。
- `missing`：当前没有公开运行时接口提供该能力。
- `not-applicable`：问题属于适配器或消息总线，不属于 RMUX。

## 能力矩阵

| 能力 | 状态 | 公开 SDK / IPC 入口 | 语义与作用域 | 故障行为 | 自动化证据 | 对 A-001 / A-002 的影响 | 是否阻塞 R-002 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 默认及显式 endpoint | supported | `RmuxEndpoint`、`RmuxBuilder::endpoint`、`unix_socket`、`windows_pipe`、`Rmux::resolved_endpoint` | 默认发现按当前用户隔离。显式 endpoint 绕过发现 allowlist，但仍受 daemon 权限检查。Unix/macOS 使用 Unix socket，Windows 使用每用户 named pipe。 | endpoint 无效或不可达时返回 `RmuxError::Transport`；平台不兼容的显式类型返回 `Unsupported`。 | `crates/rmux-sdk/tests/discovery.rs`；adapter baseline smoke | A-001 应模拟解析后的 endpoint，而不是环境变量。A-002 可接受显式 endpoint 或使用 SDK 发现。 | 否 |
| daemon 探测与 capability 协商 | supported | `Rmux::connect`、`capabilities`、`has_capability`、`list_sessions` | `connect` 只探测，不启动 daemon。握手返回当前 daemon 的字符串 capability 标识。 | 连接、wire version 和 capability 缺失分别形成 `Transport` 或 `Unsupported`。 | `crates/rmux-sdk/src/handles/rmux.rs`；adapter baseline smoke | A-001 需要 capability 集合及 typed failure；A-002 使用可选操作前必须检查。 | 否 |
| 断线检测与 client 复用 | partial | 所有 daemon-backed 操作；`RmuxError::Transport` | transport actor 会锁存终止故障。daemon 丢失后必须新建 `Rmux` 和所有子 handle；旧 handle 不自动重连。 | 正常关闭和异常退出都会使旧操作返回 `Transport`，output subscription 不会迁移。 | `failure_cleanup_uses_existing_typed_diagnostics`；adapter baseline smoke | A-002 必须重建 client、丢弃旧 handle 并重新发现。 | 保守重连不阻塞；daemon epoch 是 R-002 候选 |
| 命名 session 创建与复用 | supported | `EnsureSession`、`EnsureSessionPolicy`、`Rmux::ensure_session` | 明确支持 `CreateOnly`、`CreateOrReuse`、`ReuseOnly`。并发 `CreateOrReuse` 收敛到一个 session，且只有一个调用报告创建。 | 重复创建和复用不存在 session 均返回协议错误。 | `create_only_duplicate_is_error_but_attach_if_exists_reuses`；`concurrent_create_or_reuse_creates_one_named_session` | A-001 模拟创建结果；A-002 可安全 ensure 命名 session。 | 否 |
| window / pane 发现 | supported | `Session::window`、`Session::pane`、`Session::pane_by_id`、`Rmux::find_panes`、`Pane::id` | slot handle 每次调用都解析当前占用者；by-id handle 在当前 daemon 生命周期内绑定一个 `PaneId`。 | 不存在的 slot 按接口约定返回空/default；不存在的稳定 id 返回 `PaneNotFound` 或 stale 观察。 | `crates/rmux-sdk/tests/lifecycle.rs`；adapter baseline smoke | A-002 应缓存 `(endpoint, session name, PaneId)`，不能只缓存 pane index。 | 否 |
| 稳定对象身份 | partial | `SessionId`、`WindowId`、`PaneId`、`InfoSnapshot` | ID 只在单个 daemon 进程中稳定。daemon 重启后可能复用相同数值；当前没有公开 daemon epoch 对 ID 作限定。 | 旧 handle 返回 `Transport`；新 client 可能看到数值相同但对象全新的 ID。 | `crates/rmux-sdk/tests/identity.rs`；adapter baseline smoke | A-001 必须包含适配器连接 epoch；A-002 重连后必须清空全部身份缓存。 | 候选：若需要跨重启比较，应公开 daemon instance identity |
| generation、revision 与 sticky 状态 | partial | `SessionInfo`、`WindowInfo`、`PaneInfo`、`Pane::info` | `generation` 记录观察到的变更，`revision` 是更粗的可见/布局计数，`output_sequence` 作用于单 pane。它们是 sticky 状态计数器，不是 incarnation token，并随 daemon 重置。 | lag 后可刷新当前状态，但无法证明跨重启连续性，也无法重建丢失原始字节。 | `crates/rmux-sdk/tests/info.rs`；`crates/rmux-sdk/tests/lifecycle.rs` | A-001 将计数器与身份分开；A-002 不能仅靠 generation 判断对象替换。 | MVP 不阻塞；相关候选仍是 daemon epoch |
| pane 替换与 stale handle | supported | slot `Pane`、`Session::pane_by_id`、`Pane::id`、`Pane::info` | 同一 slot 中 pane 关闭并重建后产生新 `PaneId`。slot handle 会跟随替换对象，旧 by-id handle 保持 stale。 | 稳定 handle 的操作会失败或返回 stale/空结果，不会误指向替换 pane。 | `pane_close_and_respawn_preserve_slot_semantics`；adapter baseline smoke | A-002 的关键观察必须使用 by-id handle，stale 后重新解析。 | 否 |
| 渲染 snapshot | supported | `Pane::snapshot`、`PaneSnapshot` | 捕获当前 typed terminal grid、cell、尺寸、cursor 和 revision。它是渲染状态，不是字节 transcript，并受 IPC frame 上限约束。 | stale slot 返回 revision 为 0 的空 snapshot；transport/protocol 错误保持 typed。 | `crates/rmux-sdk/tests/snapshot.rs`；`crates/rmux-sdk/tests/smoke_v1_full.rs` | A-001 需要 rendered snapshot DTO；A-002 可在 lag 后用它重新锚定可见状态。 | 否 |
| observe-only 原始输出 | supported | `Pane::output_stream_starting_at`、`PaneOutputStream`、`PaneOutputChunk` | 观察过程不发送 pane 输入。chunk 保留任意字节，并携带单 pane 单调 sequence。 | stale pane、transport 丢失或 capability 拒绝会使订阅建立失败。 | `streams_contract_tests.rs`；`adapter_baseline_observes_replacement_and_restart_boundaries` | 这是 A-002 的主数据通路。 | 否 |
| stream 起点 cursor | partial | `PaneOutputStart::{Now, Oldest}` | `Now` 只观察未来输出；`Oldest` 重放保留环。当前没有公开任意 sequence cursor。 | retention 缺口产生 lag notice；重连时无法精确请求先前持久化的 sequence。 | `streams_contract_tests.rs`；burst / oldest smoke | A-001 只模拟 `Now` / `Oldest`；A-002 应记录 snapshot + 重新订阅的恢复方式。 | 精确恢复的候选，不阻塞 observe-only MVP |
| sequence、任意字节与 burst | supported | `PaneOutputChunk::Bytes { sequence, bytes }` | sequence 在单 pane、单 daemon 生命周期内单调。字节可包含 NUL 或非法 UTF-8。burst 保序；line rendering 是独立的 lossy 层。 | subscription/response 关联错误会被协议检查拒绝。 | `output_stream_preserves_arbitrary_bytes_and_sequences`；`immediate_burst_output_is_available_from_oldest_cursor` | A-001 必须不经 UTF-8 转换地保留 bytes 和 sequence。 | 否 |
| EOF 与 subscription 移除 | partial | 空的 `PaneOutputChunk::Bytes`；`PaneOutputStream::next -> Ok(None)` | 空字节事件被视为 pane EOF；subscription 被移除后也以 `None` 结束。公开 stream 没有区分两者的终止原因 enum；transport 丢失仍是错误。 | 调用方可区分 clean end 和 transport error，但消费终止项后不能区分 EOF 与 subscription-gone。 | `collect_stream_until_eof`；`output_stream_returns_none_when_subscription_gone` | A-002 应保守映射终止状态，并用 `info()` 确认 sticky pane state。 | 候选：typed terminal reason |
| lag 可见性 | supported | `PaneOutputChunk::Lag(PaneLagNotice)` | notice 包含 expected、resume、missed、newest sequence 和有界 recent 原始字节。stream 从保留输出继续，不会静默 lag。 | 超出 retention 的旧字节无法恢复。 | `output_stream_surfaces_lag_notice_with_recent_bytes`；`output_stream_lag_after_buffered_bytes_returns_buffered_first` | A-002 可进入 `degraded`，刷新 info/snapshot 后继续。 | 否 |
| 精确 lag / 重连恢复 | partial | `Pane::info`、`Pane::snapshot`、新 output subscription | 可刷新 sticky 和渲染状态；不能重建已丢失原始字节，重连后也没有任意 cursor。 | retention 已丢失时恢复明确是 lossy。 | `info_snapshot_lag_recovery_refreshes_sticky_state_and_output_anchor`；stream contract tests | A-002 health 必须暴露 degraded history，不能声称 exactly-once observation。 | 只有要求精确 transcript 时才是候选 |
| daemon 正常/异常重启 | partial | `Rmux::shutdown`、新的 `RmuxBuilder::connect_or_start` | 正常或异常丢失都会使 client、handle、stream、identity、counter 和 subscription 全部失效。新 client 可恢复 endpoint，但不能恢复旧 runtime。 | 旧 actor 锁存 `Transport`；bootstrap 可恢复 stale socket。 | `failure_cleanup_uses_existing_typed_diagnostics`；adapter baseline smoke | A-002 状态流为 `healthy -> disconnected -> reconnecting -> rediscovering`，绝不复用旧 handle。 | 候选：公开 daemon epoch；保守清空方案不阻塞 |
| 统一实时 `PaneEvent` 生命周期流 | missing | `PaneEvent` 仅是公开 DTO | Rust SDK 没有一个公开订阅同时产生 output、close、lag、disconnect、permission 和 command event，不能声称存在。 | 调用方必须组合 output stream、sticky polling 和 transport error。 | 公开 API 盘点；`crates/rmux-sdk/tests/events.rs` 只证明序列化 | A-001 可独立定义 adapter event；A-002 不能依赖不存在的 producer。 | 若 polling 不足则作为候选 |
| literal / key 输入 | supported | `Pane::send_text`、`Pane::send_key` | 每次调用发送一个 daemon 请求并等待响应，但没有 child process 的执行回执和 idempotency key。transport 结果不确定时重试可能产生重复副作用。 | stale pane 与 transport 丢失返回 typed error。 | `crates/rmux-sdk/tests/pane_input.rs` | R-001 只盘点；A-002 observe-only 不得调用。 | 否 |
| broadcast 输入 | partial | `Rmux::broadcast`、`PaneSet::broadcast`、`PartialBroadcastFailure` | 可报告逐 pane 成功/失败，但 batch 不具原子性，成功 pane 可能已经消费输入。 | partial failure 包含成功和失败项；盲目重试会重复成功项。 | `broadcast_reports_partial_failures_by_pane`；`crates/rmux-sdk/tests/pane_set.rs` | 排除在 A-002 observe-only 行为之外。 | 否 |
| 持久 queued input / 幂等回执 | missing | 无 | RMUX 没有公开持久输入队列、message id、ack ledger 或 exactly-once 重试契约。 | 调用方无法安全判断不确定写入是否到达 child process。 | 公开 API 盘点 | 未来投递集成必须单独设计，不能靠重试 `send_text` 模拟。 | 除非另行批准通用 terminal API，否则不属于 R-002 |
| native inbox / structured hook | not-applicable | 无 | 这是适配器/消息总线的投递模式，不是 terminal multiplexer 能力。 | 由下游 transport 定义。 | 下游 A 系列测试 | 在 RMUX 之外实现。 | 否 |
| safe stdin | not-applicable | 无 | safe stdin 需要 RMUX 原始输入之上的策略、readiness、去重和 operator 控制，明确不属于 R-001。 | 原始输入重试可能产生重复副作用。 | 仅限未来独立 Goal | 必须在 observe-only 和 native inbox 之后作为最后投递模式。 | 否 |

## 适配器就绪结论

Rust SDK 足以支持 A-001 mock 和 A-002 observe-only 适配器：endpoint 解析、capability 协商、幂等命名 session ensure、daemon 生命周期内稳定 pane identity、sticky info、渲染 snapshot、带 sequence 的原始字节、显式 lag 和 typed transport failure 都已公开。

A-002 必须采用保守恢复规则：任何 transport failure 都创建新的 adapter connection epoch，丢弃所有 RMUX handle 和缓存身份，使用新 `Rmux` 重连，重新发现命名 session/pane，刷新 `info()` 与 `snapshot()`，再创建新的 `Now` 或 `Oldest` stream。lag 或断线后必须报告历史已 degraded，不能声称拥有精确 transcript。

该基线不阻塞下游 native-inbox。native inbox 不需要 terminal 输入；它仍需要独立适配器的 endpoint/context 映射、health model 和消息总线 delivery contract，这些都不应成为 RMUX 产品能力。

## 与早期集成假设的差异

1. `generation` 是 mutation counter，不是跨重启 incarnation token。
2. `PaneId` 只在单个 daemon 进程中稳定，重启后可复用数值。
3. output subscription 只支持 `Now` 和 `Oldest`，不支持任意持久 sequence cursor。
4. `PaneEvent` 是 typed vocabulary，不是公开统一实时 event source。
5. EOF、subscription removal 和 transport loss 没有统一 typed terminal-reason enum。

## Python binding 差异

独立发布的 `librmux 0.6.1` Python SDK 是 CLI/control-mode wrapper，不是 Rust detached-IPC client 的 binding。它公开 binary contract capability JSON、命名 session ensure、字符串 pane id、capture 和同步 control-mode output；但没有 Rust SDK 的 typed `SessionId`/`WindowId`/`PaneId` DTO、sticky `InfoSnapshot` generation/revision、单 pane output sequence、`Now`/`Oldest`、结构化 lag notice 或 detached-IPC terminal failure 语义。其 control stream 支持原始字节和 EOF，但不与 `PaneOutputStream` 精确等价。

首个适配器因此应以 Rust SDK 为目标。Python parity 是独立的版本化兼容项目，不是 R-001 的默认假设。

## R-002 候选项

只有 A-001/A-002 证明以下通用缺口确有必要时，才应启动 R-002：

1. 对 object id 和 counter 作限定的公开 daemon-instance identity。
2. 用于精确重连恢复的任意 sequence 起点 cursor。
3. 区分 pane EOF、subscription removal 和 daemon shutdown 的 typed output terminal reason。
4. 如果 output + sticky polling 不足，再增加公开 lifecycle stream。

启动保守 observe-only 实现不依赖上述任何一项。
