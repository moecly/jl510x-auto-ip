# auto-ip

JL510x 菊花链的 TTL 分配 IP 工具。CLI 与 Tauri 2 GUI 共用协议实现。

## 结构

- `src/lib.rs`：帧构造、CRC、回包解析、网卡选择、运行循环（CLI/GUI 共用）。
- `src/main.rs`：CLI 薄壳，负责参数和表格输出。
- `src-tauri/`：Tauri 2 GUI 后端；网卡枚举、运行线程、事件发送。
- `ui/`：纯静态前端，无 npm 构建。
- `src/ffi.rs`：C ABI，导出 `auto_ip_*` 符号；`include/auto_ip.h` 是配套头文件。
- `.github/workflows/release.yml`：`v*` tag 触发 Linux/Windows 构建并发布 Release（CLI/GUI）。

## 常用命令

```bash
cargo test
nix develop --command just gui-build
nix develop --command just gui
# C 库产物：target/release/libauto_ip.so / libauto_ip.a
# 头文件：include/auto_ip.h
cargo build --release
```

Linux 上 GUI 必须在 X11 下渲染；代码启动时已设置 `WEBKIT_DISABLE_DMABUF_RENDERER=1` 和 `GDK_BACKEND=x11`，`just gui` 另有兜底。

Windows GUI 通过 manifest 请求管理员权限。原始帧发送和网卡枚举需要 Npcap 驱动；只复制 `Packet.dll`/`wpcap.dll` 不够，程序目录中的旧 DLL 会覆盖系统版本。Release 包内的 `install-npcap.ps1` 用于下载安装 Npcap。
