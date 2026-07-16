# Clash API 强类型客户端实施计划

- 日期：2026-07-17
- 状态：待实施
- 范围：`crates/clash-api`；后续迁移 `../../clash-nyanpasu`
- 参考：MetaCubeX API 文档、Mihomo 全局 API 配置、`clash-nyanpasu` 现有实现

## 1. 结论

采用以下总体方案：

1. 使用一个长期复用的 `reqwest::Client` 统一承载 HTTP、HTTPS、Unix socket 和 Windows named pipe 请求。
2. 使用 `reqwest-websocket` 在同一个 reqwest client 上完成 WebSocket Upgrade，从而复用 transport、鉴权、TLS 与超时配置。
3. HTTP API 使用专用方法、专用 Query/Body/Response DTO，不把 method、path、`serde_json::Value` 暴露给普通调用方。
4. 本次实现中，WebSocket API 直接返回 `reqwest_websocket::WebSocket`，不增加二次包装。
5. 采用 BackON 实现操作级退避和重试；禁用 reqwest 的应用级自动重试，避免双重重试和不可预测的请求次数。
6. 低层 client 只重试 HTTP 请求和 WS 握手，不在已经建立的 WS 内隐式重连。长连接监督和重连仍由应用层 actor 负责。
7. TODO：后续如确认调用方普遍需要强类型 WS 消息流，再增加可选的 `TypedWebSocket<T>` 适配层；不得替换或隐藏原始 `WebSocket` API。

本文将用户所说的 `blockoff` 理解为 `backoff` crate。若实际指另一个依赖，需要在实施前重新确认。

## 2. 文档约束

### 2.1 Traffic API

`/traffic`：

- 支持 HTTP GET 和 WebSocket；
- 每秒推送一次；
- `up`、`down` 的单位是字节/秒；
- `upTotal`、`downTotal` 的单位是字节。

因此 HTTP GET 也应建模为流，而不是一次性 JSON 响应。

### 2.2 Transport 与鉴权

Mihomo 支持：

- `external-controller`：HTTP；
- `external-controller-tls`：HTTPS；
- `external-controller-unix`：Unix socket；
- `external-controller-pipe`：Windows named pipe。

HTTP/HTTPS 使用 `Authorization: Bearer <secret>`。文档明确说明 Unix socket 和 named pipe 不验证 secret，因此本地 transport 不应发送鉴权头。

`external-controller-cors` 是浏览器边界配置，与原生 Rust client 无关。

## 3. 现有实现审计

### 3.1 `crates/clash-api`

当前 `client.rs` 已具备：

- `Host::{Http, UnixSocket, NamedPipe}`；
- transport 对应的 reqwest client 构造；
- base URL 归一化；
- 相对 endpoint 解析；
- GET/POST/PUT/PATCH/DELETE request builder。

仍缺少：

- secret 和敏感 header；
- 强类型 API 方法与 DTO；
- JSON body/query/response 的统一处理；
- HTTP 流式 JSON 解码；
- WebSocket Upgrade 与原始 WS client 返回；
- 结构化错误；
- retry/backoff 策略注入；
- transport 集成测试。

### 3.2 `clash-nyanpasu`

现有 REST 实现位于 `backend/tauri/src/core/clash/api.rs`：

- 通过 `Config::clash()` 隐式获取地址和 secret；
- 使用 free functions 暴露 API；
- 每次调用 `perform_request` 都重新构造 reqwest client；
- 动态路径以字符串 `format!` 拼接；
- 公开错误主要是 `anyhow::Error`。

现有 WS 实现位于 `backend/tauri/src/core/clash/ws.rs`：

- 自行构造 `ws://...` URL 和 Authorization；
- 将 WebSocket 转换为 `Receiver<T>`；
- JSON 解析、历史记录、UI 广播和重连生命周期耦合在同一模块；
- `ClashWsTraffic` 仅保留 `up/down`，遗漏 `upTotal/downTotal`；
- 连接失败后固定等待一秒并无限重连。

现有 REST 聚合重试使用全局 `CLASH_API_DEFAULT_BACKOFF_STRATEGY`。迁移后应由 composition root 注入策略，避免新 client 依赖全局状态。

## 4. 目标与非目标

### 4.1 目标

- G1：所有普通 HTTP API 的入参、出参使用强类型；WS 本次仅保证入参强类型，出参暂时为原始 `WebSocket`。
- G2：HTTP、HTTPS、Unix socket、named pipe 共用一致的 endpoint API。
- G3：WS 方法直接返回可读写和关闭的 `reqwest_websocket::WebSocket`。
- G4：HTTP stream 产出强类型领域消息；WS frame 的强类型适配列为后续 TODO。
- G5：请求重试、超时、TLS 和 WS 配置可显式注入。
- G6：secret 不进入 URL、日志、Debug 输出或错误信息。
- G7：client 可被 `clash-nyanpasu` actor 通过依赖注入复用。

### 4.2 非目标

- 不在 `clash-api` 内创建 actor。
- 不在低层 WS client 中保存历史记录或广播 UI 事件。
- 不在已经建立的 WS 连接中自动重连。
- 不为协议明确允许任意 JSON 的接口强造封闭 schema。
- 不一次性迁移 `clash-nyanpasu` 的全部调用方；迁移按 endpoint 组分阶段进行。

## 5. 总体架构

```text
clash-nyanpasu actor / core manager
        │
        │ injected Client
        ▼
clash_api::Client
  ├─ typed endpoint methods
  ├─ request executor + retry policy
  ├─ HttpStream<T>
  └─ reqwest_websocket::WebSocket
        │
        ▼
shared reqwest::Client (HTTP/1.1)
  ├─ TCP HTTP/HTTPS
  ├─ Unix socket
  └─ Windows named pipe
```

建议的 crate 布局：

```text
crates/clash-api/src/
├── lib.rs
├── client.rs          # Client / ClientBuilder
├── endpoint.rs        # ControllerEndpoint / URL 与 path 构造
├── auth.rs            # Secret / Authorization header
├── error.rs           # ApiError / ErrorBody
├── retry.rs           # RetryPolicy / BackON 适配
├── stream.rs          # HttpStream<T>
├── websocket.rs       # WebSocket Upgrade / handshake
└── api/
    ├── mod.rs
    ├── traffic.rs
    ├── logs.rs
    ├── memory.rs
    ├── connections.rs
    ├── configs.rs
    ├── proxies.rs
    ├── providers.rs
    ├── rules.rs
    ├── dns.rs
    ├── storage.rs
    └── update.rs
```

DTO 与对应 endpoint 放在同一模块，避免建立难以导航的巨大 `types.rs`。

## 6. 公开 API 设计

### 6.1 Endpoint

```rust
#[non_exhaustive]
pub enum ControllerEndpoint {
    Http(Url),
    UnixSocket(PathBuf),
    NamedPipe(PathBuf),
}
```

提供符合配置文件写法的构造器：

```rust
ControllerEndpoint::http("127.0.0.1:9090")?;
ControllerEndpoint::https("127.0.0.1:9443")?;
ControllerEndpoint::url("https://controller.example.com/api/")?;
ControllerEndpoint::unix_socket("mihomo.sock");
ControllerEndpoint::named_pipe(r"\\.\pipe\mihomo");
```

HTTP 构造器接受 `host:port` 和完整 URL。内部始终保存 `Url`，不在请求时反复解析字符串。

### 6.2 ClientBuilder

```rust
let client = Client::builder(endpoint)
    .secret(secret)
    .retry_policy(ExponentialRetry::conservative())
    .configure_reqwest(|builder| {
        builder
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
    })
    .websocket_config(ws_config)
    .build()?;
```

约束：

- `Client` 可廉价 `Clone`，所有 clone 共享连接池和策略；
- `ClientBuilder` 不要求 `Clone`；
- `.configure_reqwest` 使用 `FnOnce(ClientBuilder) -> ClientBuilder`，允许调用方注入自定义 CA、TLS 和网络参数；
- 默认 `.no_proxy()`，防止控制 API 进入系统代理或产生代理环路；
- 默认 `.http1_only()`，确保同一个 client 可以完成 WebSocket HTTP/1.1 Upgrade；
- 默认禁用 reqwest 应用级 retry；
- secret 仅对 HTTP/HTTPS 加入 header；
- header 必须标记为 sensitive；
- `Secret` 的 `Debug` 和 `Display` 不输出明文。

### 6.3 强类型 Endpoint 方法

调用方只使用业务方法：

```rust
let version: Version = client.version().await?;
let configs: RuntimeConfig = client.configs().await?;

client
    .select_proxy(ProxyName::from("GLOBAL"), ProxyName::from("DIRECT"))
    .await?;

let delay: Delay = client
    .proxy_delay(
        ProxyName::from("node"),
        DelayQuery::new(test_url, Duration::from_secs(5)),
    )
    .await?;
```

动态路径必须通过 `Url::path_segments_mut().push(...)` 构造，保证代理名、provider 名和 connection id 被正确编码。

内部可以保留泛型 request executor，但不把任意 method/path 接口作为主要公开 API。

### 6.4 Traffic 类型

```rust
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Traffic {
    pub up: BytesPerSecond,
    pub down: BytesPerSecond,
    pub up_total: Bytes,
    pub down_total: Bytes,
}
```

`Bytes` 与 `BytesPerSecond` 使用 `#[serde(transparent)]` newtype，并提供：

- `get() -> u64`；
- `From<u64>`；
- `Display`；
- 与现有 UI DTO 转换所需的 `From` 实现。

只有在单位或语义容易混淆时才使用 newtype。普通布尔值、端口等继续使用标准 Rust 类型，避免过度包装影响人体工学性。

### 6.5 HTTP stream

```rust
pub async fn traffic(&self) -> Result<HttpStream<Traffic>, ApiError>;
```

`HttpStream<T>`：

- 持有 response body stream；
- 正确处理任意 HTTP chunk 边界；
- 按 JSON 行解码；
- 实现 `Stream<Item = Result<T, ApiError>>`；
- Drop 即取消读取；
- 建立响应后不自动重连。

同一抽象复用于 `/logs`、`/memory` 和需要流式 GET 的接口。

### 6.6 WebSocket client

```rust
pub async fn traffic_ws(
    &self,
) -> Result<reqwest_websocket::WebSocket, ApiError>;
```

本次不定义 `WsClient<T>` 或 `TypedWebSocket<T>`：

- 调用方直接获得 `reqwest_websocket::WebSocket`；
- 保留完整的 `Stream<Item = Result<Message, Error>>` 与 `Sink<Message>` 能力；
- 调用方可以直接处理 Text、Binary、Ping、Pong 和 Close；
- 不隐藏 negotiated protocol、主动关闭和底层错误；
- Clash API client 仅负责 transport、鉴权、query、握手及握手阶段 retry。

代价是 WS frame 的返回类型暂时不是领域强类型，调用方需要把 Text/Binary frame 反序列化为 `Traffic`、`Memory`、`LogEntry` 等 DTO。

> TODO(ws-typed-stream)：在原始 `WebSocket` API 稳定且迁移完成后，评估增加可选的 `TypedWebSocket<T>` 或 `WebSocketJsonExt`。该适配必须是 opt-in，并保留直接返回/取回原始 `reqwest_websocket::WebSocket` 的能力。

### 6.7 Logs 的类型分支

标准日志与 `?format=structured` 的响应 schema 不同，使用两个显式方法：

```rust
client.logs(LogQuery { level }).await?;            // HttpStream<LogEntry>
client.logs_ws(LogQuery { level }).await?;         // WebSocket
client.structured_logs(...).await?;                // HttpStream<StructuredLogEntry>
client.structured_logs_ws(...).await?;             // WebSocket
```

不使用一个由运行时 `format` 决定返回类型的弱类型方法。

### 6.8 任意 JSON 接口

`/storage/key` 按协议允许任意 JSON，使用泛型保持调用端强类型：

```rust
pub async fn storage_get<T: DeserializeOwned>(
    &self,
    key: StorageKey,
) -> Result<Option<T>, ApiError>;

pub async fn storage_put<T: Serialize + ?Sized>(
    &self,
    key: StorageKey,
    value: &T,
) -> Result<(), ApiError>;
```

仅诊断信息或协议真正开放的扩展字段允许使用 `serde_json::Value`。

## 7. 错误模型

公开 API 不返回 `anyhow::Error`：

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ApiError {
    InvalidEndpoint { /* ... */ },
    UnsupportedTransport { /* ... */ },
    BuildClient(#[source] reqwest::Error),
    Transport(#[source] reqwest::Error),
    Timeout { operation: Operation },
    HttpStatus {
        status: StatusCode,
        body: Option<ErrorBody>,
    },
    Decode {
        endpoint: EndpointId,
        source: serde_json::Error,
    },
    WebSocketHandshake(#[source] reqwest_websocket::Error),
    WebSocketProtocol(#[source] reqwest_websocket::Error),
}
```

要求：

- 保留 `source` error；
- HTTP 非成功状态读取有限长度的错误 body；
- 优先解析 Mihomo `{ "message": ... }`，否则保存截断后的文本；
- 错误和 tracing span 不包含 secret；
- 错误类型提供可供 retry classifier 使用的稳定分类方法；
- 无 body 的 204 endpoint 返回 `Result<()>`。

## 8. 重试方案评估

### 8.1 reqwest 0.13 内建 retry

优点：

- client 级配置；
- host scope；
- 每请求最大重试次数；
- 默认 20% 额外请求预算；
- 可按 method、status 和 transport error 分类。

限制：

- 当前实现没有指数退避、sleep 或 jitter，retry future 是立即完成；
- 只覆盖 reqwest 发送请求和收到响应头的阶段；
- 无法覆盖 JSON body 解码或完整 typed operation；
- 无法覆盖 WS 建立后的流错误；
- 自定义 retry policy 会覆盖 reqwest 默认的 protocol-NACK policy；
- 请求 body 不可 clone 时无法重试。

结论：不作为 Clash API 的主要重试实现。

### 8.2 `backoff` crate

优点：

- 指数退避与随机化成熟；
- 支持 transient/permanent error；
- 支持最大 elapsed time 和 Retry-After。

限制：

- 当前主版本仍为 0.4；
- API 相对陈旧；
- 注入 sleeper、classifier 和 notify 不如 BackON 直观；
- 现有 `clash-nyanpasu` 已经使用 BackON，引入后会形成两个重试抽象。

结论：不新增。

### 8.3 BackON

优点：

- async API 简洁；
- 支持 exponential/constant/fibonacci；
- 支持 jitter；
- 支持 `when`、`notify`；
- 可注入 sleeper，便于测试；
- 现有 `clash-nyanpasu` 已采用。

结论：选用 BackON 1.x，但不把 BackON 类型泄漏到主要 endpoint 方法签名。

## 9. RetryPolicy 设计

```rust
pub trait RetryPolicy: Send + Sync + 'static {
    fn delays(
        &self,
        request: &RequestMetadata,
    ) -> Box<dyn Iterator<Item = Duration> + Send>;

    fn is_retryable(
        &self,
        request: &RequestMetadata,
        error: &ApiError,
    ) -> bool;
}
```

内置实现：

- `NoRetry`：默认；
- `ExponentialRetry`：内部使用 BackON 生成 delay；
- 测试用 `FixedRetry` 或 fake policy。

`ExponentialRetry::conservative()` 建议参数：

- 最多额外尝试 3 次；
- 最小延迟 100ms；
- 最大延迟 2s；
- 启用 jitter。

默认可重试：

- GET/HEAD；
- WebSocket 握手；
- connect refused、connection reset、timeout；
- 502、503、504。

默认不可重试：

- 400、401、403、404；
- schema/JSON decode；
- 已经开始消费的 HTTP stream；
- 已经建立的 WS stream；
- POST、PATCH；
- 未显式标记为可安全重复的业务操作。

PUT/DELETE 即使在 HTTP 语义上通常幂等，也需由 endpoint 元数据决定是否允许自动重试。比如“删除所有连接”和“触发 provider 更新”不应仅凭 method 推断业务安全性。

每次 retry 通过 tracing 记录 endpoint id、attempt、delay 和错误分类，不记录请求 body、header 或 secret。

## 10. WebSocket 重连边界

`traffic_ws()` 的 retry 只覆盖：

1. 构造 upgrade request；
2. 建立 transport；
3. HTTP Upgrade handshake；
4. 转换为 `reqwest_websocket::WebSocket`。

一旦方法返回 `reqwest_websocket::WebSocket`，其断线不会在内部产生一个新 socket。原因：

- 调用方可能持有 split sink/stream；
- 重连会产生数据空洞；
- 重新鉴权和 endpoint 变更需要应用层最新配置；
- 重连生命周期属于长连接 actor，而不是低层协议 client。

`clash-nyanpasu` 的 WS actor 应持有 cloneable `clash_api::Client`，在 stream 关闭后按注入的 reconnect policy 再次调用 `traffic_ws()`。

## 11. Endpoint 实施顺序

### P0：Transport + WebSocket PoC

任务：

- 使用 `reqwest-websocket` 和现有 `reqwest::Client` 完成 TCP WS Upgrade；
- Windows named pipe WS Upgrade；
- Unix socket WS Upgrade；
- HTTPS + 自定义测试 CA WS Upgrade；
- 确认 `.http1_only()` 对 REST 没有行为回归。

验收：

- 三种 transport 均能完成 REST 请求；
- 三种 transport 均能收到 WS JSON frame；
- HTTP/HTTPS 请求带 Bearer；
- 本地 transport 不带 Bearer。

若 reqwest-websocket 无法在 local transport 上 upgrade，再评估：

1. 直接基于 `reqwest::Response::upgrade()` + tungstenite 构造 WebSocket；
2. 为本地 transport 实现专用 connector。

不预先引入第二套 WebSocket 栈。

### P1：Client 基础层

任务：

- `ControllerEndpoint`；
- `ClientBuilder`；
- `Secret` 和 auth header；
- 共享 reqwest client；
- `ApiError`；
- 内部 typed request executor；
- `RetryPolicy` 与 BackON 适配；
- 动态 path segment 编码。

验收：

- client clone 共享连接池；
- 测试证明每个请求不会重建 client；
- secret 不出现在 Debug/error/tracing；
- retry 分类和最大次数可测试。

### P2：Traffic 垂直切片

任务：

- `Bytes` / `BytesPerSecond`；
- 完整 `Traffic` DTO；
- `HttpStream<Traffic>`；
- `traffic_ws() -> reqwest_websocket::WebSocket`；
- HTTP typed stream 与 WS raw frame 的协议一致性测试。

验收：

- HTTP 接口产生强类型 `Traffic`；
- WS Text/Binary frame 可反序列化为同一个 `Traffic` 类型；
- 四个字段全部保留；
- chunk 分割不会导致 JSON 解码错误；
- WS 方法直接返回原始 `reqwest_websocket::WebSocket`。

### P3：其余流式接口

按顺序实现：

1. `/memory`；
2. `/logs`，标准和 structured 分开；
3. `/connections`，先定义命名 DTO，逐步消除 `serde_json::Value`。

验收：

- 每组同时覆盖强类型 HTTP stream 与原始 WebSocket；
- 不复制 transport/auth/retry 代码；
- HTTP 消息 schema 错误包含 endpoint 上下文；
- 测试证明 WS frame 可以解码为对应领域 DTO。

### P4：普通 REST API

按业务组实现：

1. `version/configs/cache`；
2. `groups/proxies/providers`；
3. `rules/rule providers`；
4. `dns/storage`；
5. `restart/upgrade/debug`。

每个 endpoint 必须明确：

- method；
- path 参数类型；
- query 类型；
- body 类型；
- response 类型；
- 预期 status；
- 业务幂等性；
- 是否允许默认 retry。

### P5：迁移 `clash-nyanpasu`

任务：

- composition root 根据 runtime config 构造 `clash_api::Client`；
- 通过 actor startup arguments 注入；
- REST free functions 迁移为 client 方法；
- WS actor 使用 `clash-api` 返回的原始 WebSocket；
- 历史记录、UI 广播仍留在 actor；
- 删除 URL/token/handshake 重复逻辑；WS frame 到领域 DTO 的解析本次仍留在 actor；
- 删除全局 API backoff strategy；
- traffic UI DTO 增加 totals 或显式转换。

验收：

- 调用路径不新增 `::global()`；
- Tauri command 不直接访问 Clash API；
- actor 不暴露 raw `ActorRef`；
- 固定一秒无限重连被注入的 capped exponential reconnect policy 替代；
- 现有 UI 行为无回归。

## 12. 测试计划

### 12.1 单元测试

- endpoint 构造和 base path；
- 包含空格、斜杠、Unicode 的 path 参数编码；
- HTTP/HTTPS auth header；
- local transport 无 auth；
- secret Debug redaction；
- query 中 `Duration` 的毫秒序列化；
- Traffic 四字段反序列化；
- 错误 body 解析与截断；
- retryable/unretryable 分类；
- POST/PATCH 默认不重试；
- WS 方法返回原始 WebSocket，并保留 Text/Binary/Close/Ping/Pong 行为。

### 12.2 HTTP 集成测试

使用本地 Axum/Hyper mock server：

- typed JSON response；
- 204 empty response；
- 400 `{message}`；
- 401 不重试；
- 503 后成功；
- 流式 JSON 被任意 chunk 分割；
- 连接中断后的 error 分类。

### 12.3 Transport 集成测试

- Windows CI：named pipe REST + WS；
- Linux/macOS CI：Unix socket REST + WS；
- TCP HTTP/HTTPS REST + WS；
- 自签名 CA 通过显式 client builder 注入，而不是默认关闭证书验证。

### 12.4 Retry 测试

- 注入 fake sleeper 或 paused Tokio time；
- 断言 attempt 次数和 delay 序列；
- 断言 jitter 位于允许范围；
- 断言成功后立即停止；
- 断言 stream 建立后不会被 client 自动重连；
- 断言取消 future 会停止 retry。

## 13. 依赖调整

建议：

```toml
[dependencies]
backon = { version = "1", features = ["tokio-sleep"] }
bytes = "1"
futures-core = "0.3"
futures-util = "0.3"
http = "1"
reqwest = { version = "0.13", features = ["json", "stream"] }
reqwest-websocket = "0.6"
serde.workspace = true
serde_json = "1"
thiserror.workspace = true
tokio.workspace = true
tokio-util = { workspace = true, features = ["codec", "io"] }
url = "2"
```

最终 feature 集在 P0 后确定，避免同时保留未使用的 tungstenite/stream 依赖。

不建议引入 `secrecy` 只包装一个 secret；先实现小型 redacted newtype。若后续出现多类凭证，再统一引入专用 secret crate。

## 14. 风险与待确认项

| 风险 | 影响 | 处置 |
|------|------|------|
| `reqwest-websocket` 经 named pipe/Unix socket Upgrade 未被上游直接测试 | P0 可能失败 | P0 必须先验证；失败后基于 reqwest Upgraded 构造，不引入平行 transport client |
| HTTPS 协商 HTTP/2 导致 Upgrade 失败 | WS handshake 失败 | client 使用 HTTP/1 only；集成测试覆盖 HTTPS |
| Mihomo 不同版本字段差异 | enum/DTO 反序列化失败 | 对扩展频繁的枚举保留 Unknown/newtype；响应 struct 使用可选字段和 `#[non_exhaustive]` |
| HTTP streaming 的 frame 分隔与文档不一致 | `HttpStream<T>` 解码失败 | 用真实 Mihomo 做兼容测试；必要时支持 JSON line 与连续 JSON decoder |
| retry 重复副作用 | 核心重启、更新等操作重复执行 | endpoint 记录业务幂等性；默认仅 GET/WS handshake retry |
| client 与 actor 都重试 | 重试次数相乘 | client 只处理请求/握手；actor 只处理已建立 stream 的生命周期 |
| secret 泄漏到 URL | 日志和崩溃报告泄密 | native WS 只用 Authorization header，不使用 `?token=` |

## 15. 最终验收标准

1. HTTP、HTTPS、Unix socket、Windows named pipe 的 REST 请求通过集成测试。
2. 所有可用 transport 的 WS Upgrade 通过集成测试。
3. `/traffic` HTTP 产出完整强类型 `Traffic`；WS frame 经测试可解码为相同 DTO。
4. WS 方法直接返回 `reqwest_websocket::WebSocket`，不增加二次包装。
5. 普通 endpoint 的公开签名不使用裸 method/path、`serde_json::Value` 或 `anyhow::Error`。
6. secret 不出现在 URL、日志、Debug 和错误信息中。
7. 默认不会重试非幂等业务请求。
8. retry/backoff、超时、TLS、WS config 可通过 builder 显式注入。
9. `clash-nyanpasu` 的 WS actor 管理生命周期、frame 解码和应用事件，不重复实现 transport、鉴权和握手。
10. `cargo fmt`、`cargo clippy --all-targets -- -D warnings`、单元测试和跨平台 transport 测试通过。
11. 文档保留 `TODO(ws-typed-stream)`，后续可在不破坏 raw WebSocket API 的前提下增加强类型适配。

## 16. 参考资料

- MetaCubeX APIs：<https://wiki.metacubex.one/api/#traffic>
- MetaCubeX 全局 API 配置：<https://wiki.metacubex.one/config/general/#api>
- reqwest 0.13 retry：<https://docs.rs/reqwest/0.13.4/reqwest/retry/index.html>
- reqwest-websocket 0.6：<https://docs.rs/reqwest-websocket/0.6.0/reqwest_websocket/>
- backoff 0.4：<https://docs.rs/backoff/0.4.0/backoff/>
- BackON 1.6：<https://docs.rs/backon/1.6.0/backon/>
- 现有 REST：`../../clash-nyanpasu/backend/tauri/src/core/clash/api.rs`
- 现有 WS：`../../clash-nyanpasu/backend/tauri/src/core/clash/ws.rs`
- 现有 BackON 配置：`../../clash-nyanpasu/backend/tauri/src/core/clash/mod.rs`
