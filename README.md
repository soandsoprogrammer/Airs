# Airs · 多平台发布

Airs 是面向自媒体创作者的 macOS 多平台发布工具，可统一管理抖音、快手、小红书和微信视频号的多个账号，发布作品并跟踪任务。

Native macOS social media publisher for Douyin, Kuaishou, Xiaohongshu (RedNote), and WeChat Channels, built with Rust and GPUI Kit.

当前为 macOS Apple 芯片开发版，采用 Rust 原生界面。抖音、快手和视频号的视频流程已用真实账号验证并收到平台接收回执。图文和小红书的真实账号发布仍待验收。

平台的登录与发布流程参考 [social-auto-upload 的固定版本](docs/upstream.md)，由 Rust 适配器执行。

## 启动

开发工具链固定为 Rust 1.99.1；首次构建需要网络下载依赖。已在 macOS 26.5.1、Apple 芯片、Xcode Command Line Tools 环境构建通过。

```sh
cargo run --locked -p social-publisher
```

生成可双击启动的本机开发应用：

```sh
./scripts/package-macos.sh
```

随后打开 `target/package/Airs.app`。这是本机开发版，使用临时本地签名；尚未进行开发者签名、公证、安装包或干净设备验收。应用放在构建目录中，避免同步文件夹对应用包附加元数据导致签名校验失败。

## 使用流程

1. 在「账号管理」切到对应平台，点击「添加账号」。同一网站可添加多个账号，各自有独立登录空间。
2. 点账号卡片进入详情登录。视频号支持桌面微信快捷登录和微信扫码。一个微信管理多个视频号时，按提示选择要登录的视频号。小红书在二维码不可用时可使用应用内的官方短信登录表单。登录后默认显示平台昵称，也可以在详情中修改名称、编辑备注或查看该账号的发布任务。卡片上被省略的备注可悬停查看全文。
3. 在「素材库」导入视频或选择一组图片。图文支持 JPG、PNG 和 WebP，可在「新建发布」调整图片顺序。应用保存原文件引用，执行前和上传后检查文件是否变动。
4. 在「新建发布」选择「视频」或「图文」，选好素材并填写标题、描述与标签。按平台筛选可勾选多个发布账号，草稿和任务会保存图片顺序。视频号首版支持视频。图文单组上限为抖音 35 张、快手 31 张、小红书 18 张，跨平台发布时满足所有所选平台的限制。

   在「素材库」和「新建发布」点「预览」可逐张查看图片，也可播放 macOS 支持的 MP4、MOV 和 M4V 视频。

   视频批次选中抖音账号后，可在「抖音设置」添加和预览竖版、横版封面。留空使用平台默认封面。自定义封面会随草稿和抖音任务保存，确认平台已应用后才可提交。抖音图文使用首图作封面。
5. 每个账号建立独立任务。不同账号最多同时执行 3 个任务，同一账号按入队顺序执行。真实准备会上传文件并填写内容，然后停在最终提交前。停止后平台可能保留上传草稿。
6. 任务准备完成后，点击对应的发布按钮，核对账号和标题后确认。抖音要求短信验证时，可在应用内按提示完成。应用收到平台确认后显示「平台已接收」。

准备任务停在等待操作时会占用对应账号的队列位置。其他账号可使用空闲的并发位置。等待 30 分钟仍无操作会关闭准备会话并提示重新准备。

重启后恢复账号、素材、草稿、任务和事件。之前处于最终提交阶段的任务进入「结果待确认」，其他被中断的准备任务标为失败。可在卡片上打开对应平台的内容管理，用该账号核对后选择「确认未发布」，再手动重新准备。保留的排队任务也可以手动继续，先前的执行记录会保留。

在「任务与记录」，可删除失败、已停止或平台已接收的任务。排队或进行中的任务可先停止，结果待确认的任务可先核对平台记录。

## 浏览器与数据

默认 Chrome 路径：`/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`。可在设置中修改浏览器可执行文件路径；开发时也可用 `PUBLISHER_CHROME` 提供默认值。

默认数据目录：`~/Library/Application Support/com.RustSocialPublisher.SocialPublisher`。开发测试可通过 `PUBLISHER_DATA_DIR` 使用独立目录。

每个账号的浏览器 profile 由随机账号 ID 对应，禁止使用日常 Chrome 默认 profile。数据库只保存账号元信息和任务记录，不导出 Cookie；浏览器 profile 自身仍可能含敏感会话数据，未宣称已加密。数据不写入安装目录或 Git。

## 验证

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test --locked -p publisher-platforms -- --ignored
cargo run --locked -p social-publisher -- --browser-smoke
cargo run --locked -p publisher-platforms --example probe_douyin
```

最后三项需要本机 Chrome；抖音探测只验证空会话可访问登录页面，不登录、不上传、不提交。本地浏览器夹具测试不接触真实平台。

当前状态、验证结果与后续事项见 [开发记录](docs/verification.md)、[架构说明](docs/architecture.md) 和 [上游版本记录](docs/upstream.md)。

<a name="airs-sponsor"></a>

## 赞助

如果 Airs 对你的创作有帮助，欢迎支持项目的持续开发。

### 赞助商

赞助商名称、标志及链接展示区。

### 收款码

| 微信收款码 | 支付宝收款码 |
| :---: | :---: |
| 待添加图片 | 待添加图片 |
