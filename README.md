# raweb

一个使用 Rust 编写的多进程异步 HTTP 服务器示例项目。主进程会按 CPU 核心数 fork 工作进程，每个工作进程绑定到指定核心并监听 `3000` 端口。

## 功能特性

- 多进程模型（主进程 + 多个工作进程）
- Tokio 异步网络处理
- 基于 `socket2` 的底层 socket 配置（`SO_REUSEPORT` 等）
- 支持 `GET`/`HEAD` 请求
- 返回内置主页内容与基础错误页面（404/405）

## 运行环境

- Rust（建议使用最新稳定版）
- Linux（项目使用了 `fork`、`SO_INCOMING_CPU` 等特性）

## 快速开始

```bash
cargo run
```

服务启动后访问：

- `http://127.0.0.1:3000/` → `200 OK`
- 其他路径 → `404 Not Found`
- 非 `GET`/`HEAD` 方法 → `405 Method Not Allowed`

## 开发常用命令

```bash
# 代码格式检查
cargo fmt --all -- --check

# 静态检查（将 warning 视为错误）
cargo clippy --all-targets --all-features -- -D warnings

# 运行测试
cargo test
```
