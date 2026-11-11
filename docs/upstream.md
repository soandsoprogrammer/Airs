# 已选依赖与上游研究记录

更新日期：2026-11-11。

| 项目 | 本工程采用 | 参考与说明 |
| --- | --- | --- |
| Rust | 1.99.1 aarch64-apple-darwin | 原设备 1.96.0 无法编译 gpui-pre-macos 0.3.12 的 map_or_default 调用。只为本项目选择 1.99.1，未改用户默认工具链。 |
| GPUI Kit | =0.9.1 | [官方 v0.9.1 示例](https://github.com/longbridge/gpui-kit/blob/v0.9.1/examples/hello_world/src/main.rs)、[输入示例](https://github.com/longbridge/gpui-kit/blob/v0.9.1/examples/input/src/main.rs)、[安装说明](https://gpui-kit.com/docs/installation/)。统一使用 gpui-kit 门面，GPUI snapshot 为 0.3.12，由其发布依赖固定。 |
| chromiumoxide | =0.9.1 | [官方仓库](https://github.com/mattsse/chromiumoxide)、所下载 0.9.1 的 browser、page、element API 源码。持续处理 handler，使用 CDP 设置文件输入。 |
| SQLite | rusqlite 0.38.0，bundled | 完整依赖版本见 Cargo.lock。 |
| social-auto-upload | 参考 commit `3847fff01f5434a8a32829833163cd6723cabc59` | [固定版本 CLI 文档](https://github.com/dreammis/social-auto-upload/blob/3847fff01f5434a8a32829833163cd6723cabc59/docs/CLI.md) |

SAU 为 MIT，版权归 dreammis (c) 2023。许可保存在 [SAU-LICENSE.txt](SAU-LICENSE.txt)。应用按上游的创作者平台操作流程实现 Rust 适配器，使用本机 Chrome 和每个账号独立的登录会话。

| 平台 | 上游发布实现 | 参考流程 |
| --- | --- | --- |
| 抖音 | [DouYinVideo 和 DouYinNote](https://github.com/dreammis/social-auto-upload/blob/3847fff01f5434a8a32829833163cd6723cabc59/uploader/douyin_uploader/main.py) | 创作者中心扫码、切换发布类型、上传素材、填写标题与正文、处理发布验证 |
| 快手 | [KSVideo 和 KSNote](https://github.com/dreammis/social-auto-upload/blob/3847fff01f5434a8a32829833163cd6723cabc59/uploader/ks_uploader/main.py) | 创作者服务平台扫码、图文上传按钮和文件选择器、作品描述、发布确认 |
| 小红书 | [XiaoHongShuVideo 和 XiaoHongShuNote](https://github.com/dreammis/social-auto-upload/blob/3847fff01f5434a8a32829833163cd6723cabc59/uploader/xiaohongshu_uploader/main.py) | 创作服务平台登录、按视频和图文类型打开编辑页、上传和填写笔记 |

桌面应用先准备作品，再等待用户确认。提交后依据平台回执记录接收结果。结果无法确认时，任务保留在「结果待确认」，可从任务卡片打开平台管理页核对。

昵称读取额外核对了抖音官方页面引用的公开静态资源：

- 页面：[抖音创作者中心](https://creator.douyin.com/)
- 静态资源：`https://lf-fe-creator.douyinstatic.com/obj/douyn-creator-scm-cdn/douyin-creator-master-new/static/js/main.3eafcbf3.js`
- SHA-256：`929e5f07b8ff404ef6a8399137f4ab8060b449c94bf33ff23bcd8e84f46e437b`
- 观察到 `user_profile.nick_name`、`/aweme/v1/creator/pc/user/info/`、`/aweme/v1/creator/user/info/`，以及当前账号切换菜单 `role=menuitemradio`、`aria-checked=true` 和 `account-name` 样式名。读取在当前登录页面同源执行，只把昵称返回 Rust。未获得可信名称时保留临时编号。

GPUI Kit 源码 Apache-2.0；chromiumoxide MIT OR Apache-2.0；依赖再分发需保留各自许可。尚未引入 biliup，也未进行其许可或商业分发结论。
