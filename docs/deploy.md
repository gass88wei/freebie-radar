# 部署与运维指南

> 公网上线先阅读 [安全边界与管理入口隔离](security.md)。当前 Docker 默认只向宿主机回环地址发布端口；生产环境必须设置 `ADMIN_TOKEN`。

## Docker Compose 一键部署（推荐）

本项目使用 `node:24-alpine` 单容器运行（成品镜像 ~340 MB），内置 Node.js 原生 `node:sqlite`（WAL 模式），无需外接 PostgreSQL / Redis，且设置了 V8 堆与 SQLite 缓存限制，主服务容器实测常驻内存约 **80–100 MiB**。

```bash
git clone https://github.com/ZiChuanLan/freebie-radar.git
cd freebie-radar

# 可选：复制并编辑环境变量（配置 ADMIN_TOKEN / Telegram 推送 / 代理等）
cp .env.example .env

# 启动主服务（会自动建表；不设 ADMIN_TOKEN 时生产管理区锁定）
docker compose up -d --build
```

验证健康检查：

```bash
curl http://127.0.0.1:8788/health
```

### 可选：拉起 FlareSolverr 过 Cloudflare 护盾

大多数数据源（包括 `linux.do` RSS、OpenRouter、Epic、Steam、GamerPower、NodeSeek RSS 等）均可由容器内 `curl` / `fetch` 直连。如果你开启了需要复杂 Cloudflare 挑战的站点，可启用内置的 `cf` profile：

```bash
# 在 .env 中添加：FLARESOLVERR_URL=http://flaresolverr:8191
docker compose --profile cf up -d --build
```

> 服务已内置会话空闲 **90 秒** 自动销毁释放浏览器内存的机制，避免空闲时长期占用数百 MB RAM。

## 反向代理（Nginx / Caddy）

建议使用 Caddy 或 Nginx 配置 HTTPS 反代并设置强 `ADMIN_TOKEN`：

```caddyfile
radar.example.com {
    reverse_proxy 127.0.0.1:8788
}
```

## 常用运维命令

```bash
cd freebie-radar

# 查看实时采集与推送日志
docker compose logs -f --tail 50

# 手动全量采集一次 / 或采集指定源
docker compose exec radar node --no-warnings src/server/cli.ts collect
docker compose exec radar node --no-warnings src/server/cli.ts collect linuxdo-welfare

# 批量 AI 补审或重审（pending | recent | rejected）
docker compose exec radar node --no-warnings src/server/cli.ts ai-review 100 pending

# 重新生成今日精选（--dry-run 只预览不写入/推送）
docker compose exec radar node --no-warnings src/server/cli.ts daily --dry-run

# 升级到最新版本（数据保存在 ./data/radar.db 不会丢失）
git pull origin main
docker compose up -d --build
```

## 登录态站点接入（知乎 / 小红书 / 小黑盒）

这三类站点要求浏览器会话与前端动态签名，采用 **零 Cookie 上传** 方案：

1. 在你自己的桌面电脑或带图形界面的环境运行回环助手：
   ```bash
   SERVER_URL=https://your-radar-domain.com INGEST_TOKEN=your_token node scripts/pw-collect.mjs serve
   ```
2. 打开雷达网页「数据源运维」，点击对应源的 **「扫码登录」** 按钮：会跳往本地 `http://127.0.0.1:18789/login` 弹窗扫码；登录成功后自动采集一轮公开帖子推入 `/api/ingest`，**立即关闭浏览器释放全部内存并跳回雷达页面**。
3. 不想安装 Node / Playwright 的用户，也可在「数据源运维 → 登录态接入」中一键复制 **Tampermonkey 油猴脚本**，直接在日常浏览器里触发回传。

## 桌面版部署（无 Docker）{#desktop}

不想装 Docker 的用户可以用 Tauri 桌面版：Node.js 24 LTS 以**侧车（sidecar）单文件**随安装包分发，在本机原生运行，不依赖容器；采集、SQLite、推送逻辑与 Docker / 源码版完全一致。

### 前置依赖

- `webkit2gtk-4.1`（Debian/Ubuntu：`sudo apt install libwebkit2gtk-4.1-0 libgtk-3-0`）
- `curl`（用于采集与下载更新；桌面版自带 Node，无需单独安装 Node.js）
- 若从源码构建：Node.js ≥ 22.5、pnpm 10、Rust 工具链（仅构建 Tauri 时需要，安装成品包不需要）

侧车准备（仅源码构建需要）：

```bash
scripts/build-sidecar.sh   # 下载 Node 24 LTS linux-x64，输出 src-tauri/binaries/node-x86_64-unknown-linux-gnu
```

### 安装 .deb / AppImage

从 GitHub Releases 下载最新安装包：

```bash
# Debian/Ubuntu（自动解析依赖）
sudo apt install ./freebie-radar_*_amd64.deb

# 任意发行版（免安装）
chmod +x freebie-radar_*.AppImage
./freebie-radar_*.AppImage
```

安装后从应用菜单启动 **Resource Radar**，首次启动会自动创建数据目录并打开工作台窗口。AppImage 需要 FUSE 2；若提示无法挂载，可加 `--appimage-extract-and-run` 参数运行，或安装 `libfuse2`。

### 自启动

- 应用内「设置 → 开机自启」打开即可。
- 手动配置（无桌面环境托盘时）：

```bash
mkdir -p ~/.config/autostart
cp /usr/share/applications/freebie-radar.desktop ~/.config/autostart/
```

### 托盘

桌面版默认常驻系统托盘，右键菜单提供「打开工作台 / 暂停采集 / 立即采集 / 退出」。关闭主窗口只会隐藏窗口，采集与推送仍在后台运行；只有「退出」才会真正结束进程。

### 数据位置

| 内容 | 路径 |
|---|---|
| 数据库 / 日志 / 采集缓存 | `~/.local/share/freebie-radar`（`radar.db`、日志目录） |
| 配置（端口、更新通道等） | `~/.config/freebie-radar` |
| 环境变量覆盖 | 同上目录的 `.env`，或启动时注入 `PORT` 等变量 |

数据目录遵循 `XDG_DATA_HOME` / `XDG_CONFIG_HOME`；卸载软件包不会删除这些目录，重装或升级后数据零丢失。

### 升级

- **应用内更新（推荐）**：「设置 → 检查更新」，自动下载并安装新的 `.deb` / `.AppImage`，数据库自动迁移，数据不丢。
- **源码构建更新**：

```bash
git pull origin main
pnpm install
pnpm build
scripts/build-sidecar.sh
pnpm tauri build
sudo apt install ./src-tauri/target/release/bundle/deb/freebie-radar_*_amd64.deb
```

- **手动替换**：停掉托盘进程后直接覆盖安装新包；`~/.local/share/freebie-radar` 不受影响。

### 故障排查

**端口 8788 被占用**

- 现象：启动失败或页面打不开，日志提示 `EADDRINUSE`。
- 排查：`ss -lntp | grep 8788`（或 `lsof -i :8788`），确认是否已有雷达实例或别的服务占用。
- 解决：退出残留实例（`pkill -f freebie-radar`），或在「设置」里改监听端口；也可用 `PORT=9000 freebie-radar` 临时换端口启动。

**Node 侧车启动失败**

- 现象：托盘显示「服务未就绪」，日志出现 sidecar 退出码非 0。
- 逐项检查：
  1. 二进制可执行：`ls -l ~/.local/share/freebie-radar/...` 与 `src-tauri/binaries/node-x86_64-unknown-linux-gnu -v`，应输出 `v24.x.x`；无执行权限时 `chmod +x`。
  2. 架构/ABI：仅支持 x86_64 + glibc；ARM 设备需重新构建对应侧车。
  3. 依赖库：`ldd <node 侧车路径>` 查看是否缺库（常见缺 `libstdc++6`）。
  4. 端口冲突：同上，先排除 8788 被占用。
  5. 查看日志：`~/.local/share/freebie-radar/logs/` 下最新日志，或终端前台运行查看 sidecar stderr。
- 源码部署可重新生成侧车：`scripts/build-sidecar.sh`（可 `NODE_VERSION=24.x.y` 指定版本）。
