# 第三方组件说明

ArkCut 的许可证不替代第三方组件各自的许可。常规 Rust 与前端许可位于 licenses/rust 和 licenses/npm；本目录提供补充许可和来源。MPL 组件的未修改精确源码在 sources/non-media。

此轻量包不分发 FFmpeg、ffprobe 或固定 WebView2。视频组件由程序直接从 Gyan 的 Windows FFmpeg 构建站点下载，按锁定版本、归档与二进制哈希验证；Gyan 是第三方 Windows 构建提供者。下载组件的 LICENSE 和 README 同组件保存在用户目录。首次准备需要网络，完成后本地剪辑可离线运行。

界面使用微软 Evergreen WebView2；缺少时从微软下载有有效 Microsoft Corporation 签名的官方安装器，使用微软支持的静默安装方式。WebView2 的许可和隐私规则由微软提供，系统提示正常显示。WebView2 包含 SmartScreen；其信息处理见[微软隐私声明](https://aka.ms/privacy)及[Edge 隐私说明](https://learn.microsoft.com/en-us/microsoft-edge/privacy-whitepaper#smartscreen)。媒体处理在本地不代表微软组件没有独立联网行为。

来源：[Gyan Windows builds](https://www.gyan.dev/ffmpeg/builds/)、[微软 WebView2 分发说明](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution)、[Mozilla MPL 2.0](https://www.mozilla.org/en-US/MPL/2.0/)。旧离线包的 FFmpeg 静态依赖源码缺口仍属历史待办；本清单仅覆盖实际轻量包。
