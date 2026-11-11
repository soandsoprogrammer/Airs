use chromiumoxide::Page;
use publisher_platforms::browser::BrowserSession;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const UPLOAD: &str = "/creator-micro/content/upload";
const PUBLISH: &str = "/creator-micro/content/publish?enter_from=publish_page";
const POST_VIDEO: &str = "/creator-micro/content/post/video?enter_from=publish_page";
const EDITOR: &str = r#"<input placeholder="填写作品标题"><div class="zone-container" contenteditable="true">正文</div>"#;

async fn upload_state(page: &Page, origin: &str) -> anyhow::Result<Value> {
    let script = include_str!("../src/douyin_upload_page.js");
    let origin = serde_json::to_string(origin)?;
    Ok(page
        .evaluate_expression(format!("({script})({origin})"))
        .await?
        .into_value()?)
}

#[tokio::test]
#[ignore = "需要本机 Chrome；使用 --ignored 显式运行"]
async fn upload_ready_requires_visible_completion_and_editor() -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        const HTML: &str = "<!doctype html><meta charset=utf-8><body></body>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{HTML}",
            HTML.len(),
        );
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = [0u8; 4096];
            if stream.read(&mut request).await.is_ok() {
                let _ = stream.write_all(response.as_bytes()).await;
            }
        }
    });
    let profile = tempfile::tempdir()?;
    let mut browser = BrowserSession::launch(profile.path()).await?;
    let page = browser
        .browser
        .new_page(format!("{origin}{UPLOAD}"))
        .await?;

    let cases = [
        (
            "composer and fields alone",
            POST_VIDEO,
            r#"<input type="file" accept="video/*"><p>上传失败时，请重新上传</p>"#,
            true,
            "waiting",
            None,
        ),
        (
            "100% is still progress without completion evidence",
            UPLOAD,
            r#"<div class="layout progress-div-upload"><progress value="100" max="100"></progress></div>"#,
            true,
            "uploading",
            Some(100),
        ),
        (
            "non-leading widget class token on post-video route",
            POST_VIDEO,
            r#"<div class="layout progress-div-v2"><div>重新上传视频</div></div>"#,
            true,
            "ready",
            None,
        ),
        (
            "explicit composer button with updated text",
            POST_VIDEO,
            "<button>重新上传视频</button>",
            true,
            "ready",
            None,
        ),
        (
            "active numeric progress overrides a reupload button",
            POST_VIDEO,
            r#"<div class="progress-div-v2"><div role="progressbar" aria-valuenow="45">45%</div></div><button>重新上传视频</button>"#,
            true,
            "uploading",
            Some(45),
        ),
        (
            "active upload text overrides a reupload button",
            POST_VIDEO,
            r#"<div class="progress-div-v2"><div>正在上传</div></div><button>重新上传视频</button>"#,
            true,
            "uploading",
            None,
        ),
        (
            "current composer upload card reports percentage without a progressbar",
            POST_VIDEO,
            r#"<div class="layout upload-card-IBCFN_"><div class="uploading-container-kBnKYA"><div class="upload-progress-CMQQOJ"><div class="upload-progress-inner-qmkWAk"><span class="text-hkyzAY">55%</span></div></div></div></div><button>重新上传视频</button>"#,
            true,
            "uploading",
            Some(55),
        ),
        (
            "native progress normalizes its own maximum",
            UPLOAD,
            r#"<div class="progress-div-upload"><progress value="3" max="4"></progress></div>"#,
            false,
            "uploading",
            Some(75),
        ),
        (
            "fractional native progress does not lose an integer to floating point",
            UPLOAD,
            r#"<div class="progress-div-upload"><progress value=".29"></progress></div>"#,
            false,
            "uploading",
            Some(29),
        ),
        (
            "ARIA progress normalizes minimum and maximum",
            POST_VIDEO,
            r#"<div class="progress-div-upload"><div role="progressbar" aria-valuemin="10" aria-valuemax="50" aria-valuenow="20">上传中</div></div>"#,
            true,
            "uploading",
            Some(25),
        ),
        (
            "indeterminate or invalid bars do not invent a percentage",
            UPLOAD,
            r#"<div class="progress-div-upload"><progress></progress><div role="progressbar" aria-valuenow="155">上传中</div></div>"#,
            false,
            "uploading",
            None,
        ),
        (
            "conflicting legacy bars omit the percentage",
            UPLOAD,
            r#"<div class="progress-div-upload"><progress value="45" max="100"></progress><progress value="55" max="100"></progress></div>"#,
            false,
            "uploading",
            None,
        ),
        (
            "composer reading takes precedence over a legacy bar",
            POST_VIDEO,
            r#"<div class="progress-div-upload"><progress value="45" max="100"></progress></div><div class="upload-card-IBCFN_"><div class="uploading-container-kBnKYA"><div class="upload-progress-CMQQOJ"><div class="upload-progress-inner-qmkWAk"><span class="text-hkyzAY">55%</span></div></div></div></div>"#,
            true,
            "uploading",
            Some(55),
        ),
        (
            "conflicting composer readings omit the percentage",
            POST_VIDEO,
            r#"<div class="upload-card-IBCFN_"><div class="uploading-container-kBnKYA"><div class="upload-progress-CMQQOJ"><div class="upload-progress-inner-qmkWAk"><span class="text-hkyzAY">55%</span><span class="text-hkyzAY">60%</span></div></div></div></div>"#,
            true,
            "uploading",
            None,
        ),
        (
            "unrelated percentage outside upload card does not indicate upload",
            POST_VIDEO,
            r#"<div class="popover"><div class="uploading-container-kBnKYA"><div class="upload-progress-CMQQOJ"><div class="upload-progress-inner-qmkWAk"><span class="text-hkyzAY">55%</span></div></div></div></div>"#,
            true,
            "waiting",
            None,
        ),
        (
            "hidden and help progress are ignored",
            POST_VIDEO,
            r#"<div class="upload-card-IBCFN_"><div class="uploading-container-kBnKYA"><div class="upload-progress-CMQQOJ"><div class="upload-progress-inner-qmkWAk"><span class="text-hkyzAY" hidden>55%</span></div></div></div></div><aside class="progress-div-upload"><progress value="20" max="100"></progress></aside>"#,
            true,
            "waiting",
            None,
        ),
        (
            "current composer percentage at 100% still lacks completion evidence",
            POST_VIDEO,
            r#"<div class="upload-card-IBCFN_"><div class="uploading-container-kBnKYA"><div class="upload-progress-CMQQOJ"><div class="upload-progress-inner-qmkWAk"><span class="text-hkyzAY">100%</span></div></div></div></div>"#,
            true,
            "uploading",
            Some(100),
        ),
        (
            "100% progress does not override explicit completion",
            POST_VIDEO,
            r#"<div class="progress-div-v2"><progress value="100" max="100"></progress></div><button>重新上传视频</button>"#,
            true,
            "ready",
            None,
        ),
        (
            "completion with editor still missing",
            POST_VIDEO,
            "<button>重新上传视频</button>",
            false,
            "editorMissing",
            None,
        ),
        (
            "hidden status must not supply completion or failure",
            POST_VIDEO,
            r#"<div class="long-card-v1"><span style="opacity:0">重新上传</span><span aria-hidden="true">上传失败</span></div><template><button>重新上传视频</button></template>"#,
            true,
            "waiting",
            None,
        ),
        (
            "disabled or instructional controls are not completion",
            POST_VIDEO,
            r#"<div class="long-card-v1"><button disabled>重新上传</button></div><div class="long-card-help"><p><span>重新上传</span></p></div>"#,
            true,
            "waiting",
            None,
        ),
        (
            "processing overrides an available reupload button",
            POST_VIDEO,
            r#"<div class="progress-div-v2"><div>视频正在转码</div><progress value="100" max="100"></progress></div><button>重新上传视频</button>"#,
            true,
            "processing",
            None,
        ),
        (
            "visible upload alert overrides completion",
            PUBLISH,
            r#"<div role="alert">视频上传失败，请稍后重试</div><button>重新上传</button>"#,
            true,
            "failed",
            None,
        ),
        (
            "login dialog on upload route",
            UPLOAD,
            r#"<div role="dialog"><h2>请重新登录</h2><input placeholder="手机号"></div>"#,
            false,
            "needsLogin",
            None,
        ),
        (
            "unrecognized route cannot authorize readiness",
            "/creator-micro/home",
            r#"<div class="long-card-v1"><div>重新上传</div></div>"#,
            true,
            "waiting",
            None,
        ),
        (
            "legacy widget on publish route",
            PUBLISH,
            r#"<div class="long-card-v1"><div>重新上传</div></div>"#,
            true,
            "ready",
            None,
        ),
    ];
    for (case, path, markup, has_editor, status, percent) in cases {
        let path = serde_json::to_string(path)?;
        let suffix = if has_editor { EDITOR } else { "" };
        let body = serde_json::to_string(&format!("{markup}{suffix}"))?;
        page.evaluate_expression(format!(
            "history.replaceState(null, '', {path}); document.body.innerHTML = {body};"
        ))
        .await?;
        assert_eq!(
            upload_state(&page, &origin).await?,
            json!({"status": status, "percent": percent}),
            "{case}"
        );
    }
    assert_eq!(
        upload_state(&page, "https://creator.douyin.com").await?,
        json!({"status": "waiting", "percent": null})
    );

    browser.close().await?;
    server.abort();
    Ok(())
}
