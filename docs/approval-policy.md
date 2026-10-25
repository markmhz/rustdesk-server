# 设备审批与管理 API（本 fork 的改动）

本 fork 在 `rustdesk-server` 上增加了**设备审批策略**和**管理 API**，目标场景是「自己跨设备 / 小团队内网远控」。

**客户端不需要重新编译**，继续用官方 Release。

## 为什么客户端零改动

不能改协议。三条卡点全部复用上游既有枚举值：

| 场景 | 使用的既有枚举 | 官方客户端表现 |
|---|---|---|
| 未批准设备注册公钥 | `RegisterPkResponse.Result::NOT_DEPLOYED` | 认为服务端未部署，连不上 |
| 连接未批准设备 | `PunchHoleResponse.Failure::ID_NOT_EXIST` | 显示对方不存在 / 离线 |

## 三道卡点

全部位于 `src/rendezvous_server.rs`：

1. **`RegisterPeer`（在线登记）** — 未批准则不把地址写入内存表，设备永远不会变成「在线」。
   > 这里**必须回包**，不能静默丢弃。`hbbs` 自带存活探针 `test_hbbs`，每秒发送 `RegisterPeer{id:"(:test_hbbs:)"}`，**12 秒收不到任何回应就 `process::exit(1)`**。静默丢弃会导致开启审批后服务端在 12 秒后自杀。
2. **`RegisterPk`（公钥登记）** — 未批准则回 `NOT_DEPLOYED`，并把设备以 `status = NULL`（待审批）落库，管理员才能在列表里看到它。
3. **`PunchHoleRequest`（发起连接）** — 目标未批准则回 `ID_NOT_EXIST`。

## 策略

`--peer-policy`（或环境变量 `PEER_POLICY` / `.env` 中的 `PEER_POLICY`）：

| 策略 | 行为 | 适用 |
|---|---|---|
| `open`（默认） | 与上游一致，`decide()` 立即返回允许，零数据库查询 | 先跑通 |
| `approve` | 陌生设备自动落库为待审批并拒绝，批准后放行 | 推荐 |
| `whitelist` | 陌生设备直接拒绝且不落库，只能靠 API 预授权 | 固定设备清单 |

`peer.status` 语义（沿用 Pro 的 `disabled` 语义，**不需要改表结构**）：`NULL` 待审批、`1` 已批准、`0` 已拉黑。

## 管理 API

基于 `axum`，默认只监听 `127.0.0.1:21114`。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/health` | 免鉴权，返回策略与版本 |
| GET | `/api/policy` | 当前策略 |
| GET | `/api/peers` | 全部设备：`id` / `status` / `online` / `info` |
| POST | `/api/peers/{id}/approve` | 批准；未知 ID 会自动预建记录（可提前授权） |
| POST | `/api/peers/{id}/block` | 拉黑，未知 ID 返回 404 |
| DELETE | `/api/peers/{id}` | 彻底删除（含公钥记录） |

鉴权：`X-Api-Key: <key>` 或 `Authorization: Bearer <key>`。**未配置 `--api-key` 时 API 只允许绑定回环地址**，绑到外部会拒绝启动并报错。

```bash
curl -s -H "X-Api-Key: $KEY" http://127.0.0.1:21114/api/peers
curl -s -X POST -H "X-Api-Key: $KEY" http://127.0.0.1:21114/api/peers/123456789/approve
curl -s -X POST -H "X-Api-Key: $KEY" http://127.0.0.1:21114/api/peers/123456789/block
```

## 新增参数

```
--peer-policy=[POLICY]   open | approve | whitelist   (默认 open)
--api-port=[NUMBER]      管理 API 端口                (默认 21114)
--api-bind=[IP]          管理 API 绑定地址            (默认 127.0.0.1)
--api-key=[KEY]          API 鉴权密钥
```

## 上游已有、不必重写的

- **中继限速**（`hbbr`）：`SINGLE_BANDWIDTH`、`TOTAL_BANDWIDTH`、`LIMIT_SPEED`；运行时可用控制台命令 `sb` / `tb` / `ls`
- **强制走中继**：`ALWAYS_USE_RELAY=Y`
- **连接密钥**：`hbbs -k <key>`，客户端须一致
- **运行时控制台**：本机连接 `127.0.0.1:21115`，可用 `ib` / `ic` / `pr` 等排障命令

## 部署要点

放行 TCP `21115-21119` + UDP `21116`。

```bash
hbbs -r <公网IP>:21117 -k <KEY> --peer-policy approve --api-key <APIKEY>
```

客户端 → 设置 → 网络：填 ID 服务器、中继服务器、Key（与服务端一致）。

审批流程：启动客户端 → 注册被拒 → 管理员 `/api/peers` 看到待审批条目 → `approve` → 客户端重连或重启即可。

## 已知限制

- 审批状态缓存在进程内存：直接改 SQLite 不生效，需走 API 或重启；多实例共用一个库时缓存不同步。
- 陌生设备大量注册会持续写库，未做审批风暴限流。
- 编译期依赖 `db_v2.sqlite3` + `.env`（`sqlx::query!` 需要编译时读 schema），**这两个文件不能删**，CI 同理。
- 授权 AGPL-3.0：内网自用无约束；对外分发须开源改动或购买商业授权。
